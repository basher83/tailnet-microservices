# Troubleshooting & Known Issues

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

## Troubleshooting

### Pod Not Starting

The startup probe allows up to 60 seconds (30 failures x 2-second period) for the proxy to bind its listener and respond to `/health`. This should happen within seconds under normal conditions. If the startup probe exhausts its budget, Kubernetes restarts the container.

Check container logs for configuration errors. Common causes: missing or malformed ConfigMap, invalid `upstream_url`, or `listen_addr` already in use.

### Tailnet Not Reachable

If the proxy pod is running but not reachable via MagicDNS (`anthropic-oauth-proxy`), the issue is with the Tailscale Operator. Check that the Operator created its proxy StatefulSet from the Ingress resource:

```bash
kubectl -n anthropic-oauth-proxy get statefulset
kubectl -n anthropic-oauth-proxy get pods -l app=tailscale
kubectl -n anthropic-oauth-proxy get ingress
```

Only one Tailscale proxy pod should exist. If there are two (a symptom of dual-proxy conflict from Service annotations), ensure `k8s/service.yaml` has no `tailscale.com/expose` or `tailscale.com/hostname` annotations. The Ingress resource handles all tailnet exposure.

### Proxy Returning 502 Bad Gateway

The upstream at `https://api.anthropic.com` is unreachable or returning connection errors. Use port-forwarding to test from your workstation:

```bash
kubectl -n anthropic-oauth-proxy port-forward deployment/anthropic-oauth-proxy 8080:8080 &
curl -s -o /dev/null -w '%{http_code}' http://localhost:8080/health
```

If DNS or TLS fails inside the pod, check that the runtime image has `ca-certificates` installed (it does in the default Dockerfile) and that the pod has outbound internet access.

### Proxy Returning 504 Gateway Timeout

Upstream did not respond within the configured `timeout_secs`. The code default is 180 seconds, and the production K8s config currently sets `timeout_secs = 180`. The proxy automatically retries initial-response timeouts up to 2 times (3 total attempts) with 100ms backoff between attempts. If all attempts time out, it returns 504.

For sustained 504s, check Anthropic API status. If the API is healthy, consider increasing `timeout_secs` in the ConfigMap for long-running requests.

### Proxy Returning 400 Bad Request

Either the request body exceeds the 10 MiB hardcoded limit, or the request is malformed. Check the `request_id` in the error response JSON and correlate with proxy logs.

### Proxy Returning 429 (OAuth Mode)

In OAuth mode, the proxy attempts failover to the next available account when the current account's quota is exhausted (429 with quota message). If the last selected account returns a quota 429, that upstream response can pass through to the client. If no account can be selected because the pool is empty, cooling down, disabled, or missing credentials, the proxy returns 503 Service Unavailable.

Check pool status via the health endpoint or admin API to see which accounts are cooling down and when they will become available again. Default cooldown is 2 hours (configurable via `cooldown_secs`).

### Pool Exhausted (OAuth Mode)

When all accounts are unavailable because they are in `cooling_down` or `disabled` state, or because the pool is empty, the proxy returns 503 Service Unavailable before forwarding the request. To resolve:

- Wait for cooldown timers to expire (check `cooldown_remaining_secs` in pool health)
- Add more accounts by loading credentials through keychain extraction
- Remove and re-add disabled accounts with fresh extracted credentials (disabled means refresh token is permanently invalid)

The client-visible error embeds the pool summary, which tells you which case you are in:

```text
503 {"error":{"message":"provider error: pool exhausted: {\"error\":{\"message\":\"All accounts exhausted\",
\"pool\":{\"accounts_available\":0,\"accounts_cooling_down\":0,\"accounts_disabled\":1,\"accounts_total\":1},
\"type\":\"pool_exhausted\"}}","request_id":"req_…","type":"proxy_error"}}
```

`accounts_disabled ≥ 1` means a refresh token was rejected. Confirm and date it from the pod logs:

```bash
kubectl -n anthropic-oauth-proxy logs deploy/anthropic-oauth-proxy --since=720h \
  | grep -E 'refresh token rejected|refresh succeeded' | sed -n '1p;$p'
```

The first `refresh token rejected, disabling account` line is the outage start; the last `background token refresh succeeded` before it is when the token was last good. Anthropic's `error_description` distinguishes `Refresh token expired` (server TTL, expect ~4–6 weeks) from `Refresh token not found or invalid` (grant rotated/revoked). Either way the fix is the same: re-auth via keychain extraction ([Accounts → Refresh Token Lifetime](./accounts.md#refresh-token-lifetime-and-re-auth)). Note that `/health` still returns HTTP 200 in this state — only its `status`/`pool.status` fields say `unhealthy`, so a liveness-only check will not catch it.

### High Latency

Check `proxy_request_duration_seconds` histogram percentiles. Latency is dominated by upstream response time. The proxy adds negligible overhead (header injection, hop-by-hop stripping, JSON body modification in OAuth mode).

If latency correlates with high concurrency, check if `max_connections` (default: 1000) is being hit. The concurrency limiter queues excess requests rather than rejecting them, which manifests as increased latency rather than errors. Health and metrics endpoints are outside the concurrency limit and remain responsive regardless of proxy load.


## Known Issues

### PKCE Web Flow Failed on Request Shape, Not Policy (fixed 2026-08-26, pending deploy)

**History.** First seen ~2026-02-12: consent page rendered, clicking Authorize failed with `POST /v1/oauth/{session_id}/authorize → 400 "Invalid request format"`. The 2026-02-17 "Q13-gate" investigation (`52a8fee`, `d01eb8a`) aligned client id, redirect URI, scopes (adding `user:mcp_servers`) and S256 to CC CLI v2.1.44, saw the same failure, and recorded it as "Anthropic server-side enforcement blocking third-party OAuth consumers". That conclusion stood, untested, until 2026-08-26.

**Retest 2026-08-26 (bisected by hand-built authorize URLs on one live `init-oauth` challenge):**

| Variant | Delta from proxy's URL | Result |
|---|---|---|
| proxy as-is | — | fails **on page load**: "Authorization failed / Invalid request format" |
| A | `+ code=true` | consent renders → Authorize → "Invalid request format" |
| B | A + pi's 6 scopes | same as A |
| C | B + `redirect_uri=http://localhost:53692/callback` (pi's exact shape) | same as A |
| **D** | C + `state` = 43-char random base64url (instead of `claude-max-<ts>`) | **Authorize succeeds**, redirects to callback with `code=…&state=…` |

C and D differ only in `state`, so the Authorize POST rejection is isolated to the `state` format. The redirect URI is *not* gated: A/B (our `https://platform.claude.com/oauth/code/callback`) rendered consent identically to localhost, so the manual-paste flow can stay.

A **third** bug surfaced once the authorize step passed: the token endpoint answered `400 invalid_request: Invalid 'code_verifier'`. The proxy generated a 128-byte (171-char) verifier; RFC 7636 §4.1 caps it at 128 chars. 32 bytes (43 chars, what Claude Code and pi send) is accepted.

**Fixed 2026-08-26** in `b883966` (`code=true`, random 43-char `state`, PKCE map keyed by state, `state` echoed to the token endpoint) and the follow-up verifier-length commit. **Verified end to end** against a locally run build with an empty pool: `init-oauth` → browser Authorize → `complete-oauth` → `{"status":"added"}` → pool `healthy` → live `/v1/messages` → `ok`. Not yet deployed. The change: in `crates/anthropic-auth` / `services/oauth-proxy/src/admin.rs`, (1) `code=true` on the authorize URL; (2) `state` = 32 random bytes base64url, the in-memory PKCE entry keyed by it and returned alongside `account_id` so `complete-oauth` can look it up from the pasted `code#state`; (3) verifier shortened to 32 bytes. Reference: `earendil-works/pi` `packages/ai/src/auth/oauth/anthropic.ts:248-252` and `pkce.ts`.

**Why this matters beyond convenience:** a PKCE-provisioned account owns its own refresh-token lineage. The keychain-extraction path shares one lineage with the local Claude Code login, which is the structural cause of the ~6-week `invalid_grant` outages (see "Disabled Accounts Never Auto-Recover").

Policy note: Anthropic's current terms (https://code.claude.com/docs/en/legal-and-compliance) still say OAuth is for "ordinary use of Claude Code and other native Anthropic applications" and that developers "may not collect, store, or intermediate Claude.ai credentials." The flow working is a technical fact, not a policy clearance.

### Disabled Accounts Never Auto-Recover

Once an account is `disabled` (permanent refresh failure: `invalid_grant` / 401 / 403), nothing in the proxy re-enables it — not a pod restart with the same credential file, not a later successful refresh of another account. Recovery is always manual: overwrite `credentials.json` with freshly extracted tokens and restart ([Accounts](./accounts.md#adding-an-account-keychain-extraction)). Refresh tokens have been observed to expire ~6 weeks after extraction, so with a single-account pool this is a recurring outage unless alerted on (`accounts_disabled` alert in [Monitoring](./monitoring.md#alerts)).

### Background Refresh Keeps Retrying Disabled Accounts

`refresh_cycle` in `crates/anthropic-pool/src/refresh.rs` iterates `pool.account_ids()` without filtering on status, so a `disabled` account whose access token is past the refresh threshold is re-sent to Anthropic's token endpoint every cycle (default 5 minutes) and logs `WARN refresh token rejected, disabling account` each time. Observed 2026-08-01 → 2026-08-26: one WARN every 5 min for 25 days on an account that was already disabled. Harmless to pool state (it is already disabled) but it is log noise, a pointless call to Anthropic with a dead token, and it makes the WARN useless as an "onset" signal — use the *first* occurrence, not the latest. Code fix: skip accounts whose status is `Disabled` in `refresh_cycle`. Not yet done.

### Credential File Missing `type` Field

The credential file requires a `type` field (value: `"oauth"`) on every credential entry. Omitting it causes a fatal parse error on startup (`missing field 'type'`), putting the pod into CrashLoopBackOff. If this happens, the PVC retains the bad file across restarts.

Recovery: scale the deployment to 0 (disable ArgoCD auto-sync first if enabled), run a temporary pod mounting the PVC, fix the file, then restore. See the field reference in `specs/anthropic-oauth-gateway.md` for the required format.
