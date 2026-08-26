# OAuth Account Management

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

Accounts are managed via the admin API on port 9090. The admin port is not exposed via Ingress — access it through `kubectl port-forward`.

## Accessing the Admin API

```bash
kubectl -n anthropic-oauth-proxy port-forward deployment/anthropic-oauth-proxy 9090:9090
```

All admin commands below assume port-forwarding is active.

## Adding an Account (PKCE Flow — Broken in Proxy Code, Not Blocked)

**Status 2026-08-26:** the flow fails because of two request-shape bugs in the proxy, **not** Anthropic policy (see [Known Issues](./troubleshooting.md#pkce-web-flow-fails-on-request-shape-not-policy) for the evidence). Until they are fixed, use Keychain Extraction below.

- The authorize URL must include `code=true` (without it the page fails on load).
- `state` must be a long random base64url value (pi and Claude Code send the PKCE verifier); the proxy sends the account id (`claude-max-<ts>`), which the Authorize POST rejects with "Invalid request format".

`init-oauth` is a `POST` (the route is `post(init_oauth)` in `services/oauth-proxy/src/admin.rs`).

Step 1 — Initiate the OAuth flow:

```bash
curl -s -X POST http://localhost:9090/admin/accounts/init-oauth | jq .
```

Response:

```json
{
  "authorization_url": "https://claude.ai/oauth/authorize?client_id=...&code_challenge=...",
  "account_id": "claude-max-1739059200",
  "instructions": "Open the URL in a browser, authorize, then paste the code to complete-oauth"
}
```

Step 2 — Open the `authorization_url` in a browser and authorize with the Claude Max account. After authorization, the browser redirects to a page showing a `code#state` value.

Step 3 — Complete the flow:

```bash
curl -s -X POST http://localhost:9090/admin/accounts/complete-oauth \
  -H 'Content-Type: application/json' \
  -d '{"account_id": "claude-max-1739059200", "code": "AUTH_CODE#STATE"}' | jq .
```

The PKCE state expires after 10 minutes. If Step 3 is not completed in time, start over from Step 1.

## Adding an Account (Keychain Extraction)

If the PKCE consent flow fails (see Known Issues), credentials can be extracted from a local Claude Code installation and loaded directly. This is also the **re-auth procedure** whenever an account goes `disabled` (see [Refresh Token Lifetime](#refresh-token-lifetime-and-re-auth) below).

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

## Refresh Token Lifetime and Re-auth

Access tokens last ~8 hours and are refreshed proactively by the background task (observed cadence: one successful `background token refresh succeeded` every ~7h45m). The **refresh token** itself also expires, and when it does the account is permanently `disabled` until a human re-auths — there is no auto-recovery path in the proxy.

Observed lifetimes (from pod logs, single account `claude-max-local`):

| Loaded via keychain | First `invalid_grant` | Lifetime | Anthropic `error_description` |
|---|---|---|---|
| ~2026-06-20 | 2026-08-01 18:36Z | ~6 weeks | `Refresh token expired` |
| (earlier) | 2026-06-20 02:12Z | — | `Refresh token not found or invalid` |

Two distinct descriptions have been seen. `Refresh token expired` reads as a server-side TTL. `Refresh token not found or invalid` is more consistent with the token having been rotated away by another client (Anthropic rotates the refresh token on every successful refresh; the local Claude Code that the credential was extracted from refreshes the *same* grant independently). Neither cause is confirmed — inferred from the error text only.

Practical guidance:

- Expect to re-auth roughly every 4–6 weeks per account; plan for it rather than discovering it from 503s. See the `accounts_disabled` alert in [Monitoring](./monitoring.md#alerts).
- Symptom on the client side is `503 … "type":"pool_exhausted"` with `accounts_disabled ≥ 1` in the embedded pool summary ([Troubleshooting](./troubleshooting.md#pool-exhausted-oauth-mode)).
- Re-auth = the Keychain Extraction procedure above (with its precondition). The disabled account is replaced in place when you overwrite `credentials.json` and restart.
- Until the account is replaced, the background task logs `refresh token rejected, disabling account` every 5 minutes for the already-disabled account (known noise — see [Known Issues](./troubleshooting.md#background-refresh-keeps-retrying-disabled-accounts)).

## Credential Persistence

OAuth credentials are stored in `/data/credentials.json` on a PersistentVolumeClaim. Pod restarts preserve tokens — no need to re-authenticate accounts after restart.

The single-replica constraint exists because PKCE state is held in-memory. Running multiple pods would split the init/complete flow across pods. This does not affect credential persistence (PVC survives pod restarts).

