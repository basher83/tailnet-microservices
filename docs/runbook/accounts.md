# OAuth Account Management

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

Accounts are managed via the admin API on port 9090. The admin port is not exposed via Ingress — access it through `kubectl port-forward`.

## Accessing the Admin API

```bash
kubectl -n anthropic-oauth-proxy port-forward deployment/anthropic-oauth-proxy 9090:9090
```

All admin commands below assume port-forwarding is active.

## Adding an Account (PKCE Flow — Currently Blocked)

**Note:** This flow is currently blocked by Anthropic's server-side enforcement (see Known Issues). Use the Keychain Extraction method below instead.

Step 1 — Initiate the OAuth flow:

```bash
curl -s http://localhost:9090/admin/accounts/init-oauth | jq .
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

If the PKCE consent flow fails (see Known Issues), credentials can be extracted from a local Claude Code installation and loaded directly.

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

Step 2 — Copy the credential file into the pod:

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

## Credential Persistence

OAuth credentials are stored in `/data/credentials.json` on a PersistentVolumeClaim. Pod restarts preserve tokens — no need to re-authenticate accounts after restart.

The single-replica constraint exists because PKCE state is held in-memory. Running multiple pods would split the init/complete flow across pods. This does not affect credential persistence (PVC survives pod restarts).

