# OAuth Account Management

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

Accounts are managed via the admin API on port 9090. The admin port is not exposed via Ingress — access it through `kubectl port-forward`.

## Accessing the Admin API

```bash
kubectl -n anthropic-oauth-proxy port-forward deployment/anthropic-oauth-proxy 9090:9090
```

All admin commands below assume port-forwarding is active.

## Adding an Account (PKCE Flow)

**Status 2026-08-26:** fixed (`b883966`, `391a62a`), deployed as `sha-3b30262`, and used to provision `claude-max-1787733199` on the live proxy the same day. This is now the **preferred** provisioning path; Keychain Extraction below is the fallback. History and evidence: [Known Issues](./troubleshooting.md#pkce-web-flow-failed-on-request-shape-not-policy-fixed-2026-08-26).

Prefer this flow over keychain extraction: a PKCE-provisioned account owns a separate refresh-token lineage rather than sharing the local Claude Code login. This avoids sharing refreshes with that client, but does not establish a longer lifetime: the PKCE account also reported `Refresh token expired` after ~29.5 days (see [Refresh Token Lifetime](#refresh-token-lifetime-and-re-auth)). The PKCE state is single-use and expires **10 minutes** after `init-oauth`; complete the browser step promptly.

`init-oauth` is a `POST` (the route is `post(init_oauth)` in `services/oauth-proxy/src/admin.rs`).

Step 1 — Initiate the OAuth flow:

```bash
curl -s -X POST http://localhost:9090/admin/accounts/init-oauth | jq .
```

Response:

```json
{
  "authorization_url": "https://claude.ai/oauth/authorize?code=true&client_id=...&code_challenge=...&state=...",
  "account_id": "claude-max-1739059200",
  "state": "rBTzVG9sJ4QMkfFn8fuU5eo3qkGDzA_uNooVEbSOKIo",
  "instructions": "Open the URL in a browser, authorize, then paste the code#state value to complete-oauth"
}
```

Step 2 — Open the `authorization_url` in a browser and authorize with the Claude Max account. After authorization, the browser redirects to a page showing a `code#state` value.

Step 3 — Complete the flow. The `code#state` value is all that is needed; the proxy looks up the pending flow by `state`. `account_id` is optional and, if given, must match the flow that produced that `state`:

```bash
curl -s -X POST http://localhost:9090/admin/accounts/complete-oauth \
  -H 'Content-Type: application/json' \
  -d '{"code": "AUTH_CODE#STATE"}' | jq .
```

Response: `{"account_id": "claude-max-1739059200", "status": "added"}`. Confirm with `/admin/pool`.

The PKCE state expires after 10 minutes. If Step 3 is not completed in time, start over from Step 1.

## Adding an Account (Keychain Extraction)

If the PKCE consent flow fails (see Known Issues), credentials can be extracted from a local Claude Code installation and loaded directly. Use [PKCE](#adding-an-account-pkce-flow) for routine re-auth when an account goes `disabled`; keychain extraction remains a fallback, with a shared lineage and a credential-file overwrite.

**Precondition:** the local Claude Code install must itself be freshly logged in. Run `claude` interactively and confirm it answers a prompt *before* extracting — otherwise you copy a refresh token that is already expired and the pool disables again on the first refresh cycle. If in doubt, `claude /logout` then log in again first.

Step 1 — Extract tokens from the macOS keychain (tokens are not printed):

```bash
CREDS=$(security find-generic-password -s "Claude Code-credentials" -a "$(whoami)" -w)
echo "$CREDS" | python3 -c "
import json, sys
data = json.load(sys.stdin)
oauth = data['claudeAiOauth']
print(json.dumps({
    'claude-max-local': {
        'type': 'oauth',
        'refresh': oauth['refreshToken'],
        'access': oauth['accessToken'],
        'expires': oauth['expiresAt']
    }
}, indent=2))
" > /tmp/credentials.json
```

Step 2 — Copy the credential file into the pod. This **overwrites** `/data/credentials.json`, so a disabled account with the same ID is replaced in place (no separate DELETE needed):

```bash
POD=$(kubectl -n anthropic-oauth-proxy get pods -l app=anthropic-oauth-proxy -o name | head -1)
kubectl cp /tmp/credentials.json anthropic-oauth-proxy/${POD#pod/}:/data/credentials.json -c proxy
```

Step 3 — Restart the pod to load the new credentials:

```bash
kubectl -n anthropic-oauth-proxy rollout restart deployment/anthropic-oauth-proxy
```

Step 4 — Verify the account loaded:

```bash
curl -s http://localhost:9090/admin/pool | jq .
```

Clean up the local temp file after confirming:

```bash
rm -f /tmp/credentials.json
```

The keychain entry name varies by platform. On macOS, Claude Code stores credentials under service `Claude Code-credentials`. The `claudeAiOauth` key contains the tokens for claude.ai OAuth (Max/Pro subscriptions). The `expiresAt` field is already in unix milliseconds, matching the gateway's `expires` field directly.

## Listing Accounts

```bash
curl -s http://localhost:9090/admin/accounts | jq .
```

Response includes account IDs and status (available, cooling_down, disabled). Tokens are never exposed.

## Removing an Account

```bash
curl -s -X DELETE http://localhost:9090/admin/accounts/claude-max-1739059200 | jq .
```

Removes the account from the pool and credential store. Idempotent.

## Pool Status

```bash
curl -s http://localhost:9090/admin/pool | jq .
```

Returns per-account status, cooldown timers, and overall pool health.

## Current Pool Composition (2026-10-04)

| Account | Provisioned via | Refresh-token lineage | Role / observed state |
|---|---|---|---|
| `claude-max-1791092167` | PKCE admin flow, 2026-10-04 | proxy-owned (its own grant) | sole account; available |

Both disabled accounts, `claude-max-1787733199` (PKCE) and `claude-max-local` (keychain), were removed after adding the replacement via PKCE; see the [2026-10-04 recovery audit](../audits/OAUTH_GRANT_EXPIRY_2026-10-04.md). A single-account pool has no account failover when that grant expires.

## Refresh Token Lifetime and Re-auth

Access tokens last ~8 hours and are refreshed proactively by the background task (observed cadence: one successful `background token refresh succeeded` every ~7h45m). The **refresh token** itself also expires, and when it does the account is permanently `disabled` until a human re-auths — there is no auto-recovery path in the proxy.

Observed lifetimes (dated pod-log evidence; older rows retained as history):

| Provisioned / lineage | First `invalid_grant` (UTC) | Lifetime | Anthropic `error_description` |
|---|---|---|---|
| ~2026-06-20 | 2026-08-01 18:36Z | ~6 weeks | `Refresh token expired` |
| (earlier keychain grant) | 2026-06-20 02:12Z | — | `Refresh token not found or invalid` |
| 2026-08-26 / keychain, `claude-max-local` | 2026-09-23 07:26:55Z (inline refresh) | ~28 days | `Refresh token expired` |
| 2026-08-26 / PKCE, `claude-max-1787733199` | 2026-09-24 19:44:37Z (background refresh) | ~29.5 days | `Refresh token expired` |

Two distinct descriptions have been seen. `Refresh token expired` reads as a server-side TTL. `Refresh token not found or invalid` is more consistent with the token having been rotated away by another client (Anthropic rotates the refresh token on every successful refresh; the local Claude Code that the credential was extracted from refreshes the *same* grant independently). Neither cause is confirmed. The September failures at similar ages support a ~30-day server-side grant lifetime as an inference from timing and error text; they do not prove a fixed vendor TTL or that PKCE grants last longer than keychain grants.

Complete pool unavailability lasted from 2026-09-24 19:44:37Z to 2026-10-04 05:38:31Z (~9.4 days); this dates credential/pool unavailability, not a count of failed requests. See the [recovery audit](../audits/OAUTH_GRANT_EXPIRY_2026-10-04.md) for per-account evidence and client verification.

Practical guidance:

- Plan for re-auth on roughly a 30-day cycle per account, regardless of PKCE or keychain lineage, and arrange browser consent before the observed expiry window. This is an operational estimate, not a guaranteed lifetime. For the October 4 grant, ~2026-11-03 is the next planning window. See the proposed `accounts_disabled` alert in [Monitoring](./monitoring.md#key-alerts); the proxy metrics endpoint was not scraped in the October 4 investigation, so this is not an active alerting guarantee.
- Symptom on the client side is `503 … "type":"pool_exhausted"` with `accounts_disabled ≥ 1` in the embedded pool summary ([Troubleshooting](./troubleshooting.md#pool-exhausted-oauth-mode)).
- Re-auth via [PKCE](#adding-an-account-pkce-flow): add the new account, confirm it is available in `/admin/pool`, then remove the disabled account by its exact ID. Verify `/health` body status and an end-to-end client request; a restart with the same credentials cannot restore an expired grant. Record the first rejection and its description before removing the old account.
- Until the account is replaced, the background task logs `refresh token rejected, disabling account` every 5 minutes for the already-disabled account (known noise — see [Known Issues](./troubleshooting.md#background-refresh-keeps-retrying-disabled-accounts)).

## Credential Persistence

OAuth credentials are stored in `/data/credentials.json` on a PersistentVolumeClaim. Pod restarts preserve tokens — no need to re-authenticate accounts after restart.

The single-replica constraint exists because PKCE state is held in-memory. Running multiple pods would split the init/complete flow across pods. This does not affect credential persistence (PVC survives pod restarts).

