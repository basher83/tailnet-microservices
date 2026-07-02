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

### High Latency

Check `proxy_request_duration_seconds` histogram percentiles. Latency is dominated by upstream response time. The proxy adds negligible overhead (header injection, hop-by-hop stripping, JSON body modification in OAuth mode).

If latency correlates with high concurrency, check if `max_connections` (default: 1000) is being hit. The concurrency limiter queues excess requests rather than rejecting them, which manifests as increased latency rather than errors. Health and metrics endpoints are outside the concurrency limit and remain responsive regardless of proxy load.


## Known Issues

### PKCE Web Flow Blocked by Anthropic (Platform Constraint)

The PKCE flow's `init-oauth` endpoint generates an authorization URL for `claude.ai/oauth/authorize`. The consent page loads correctly and displays the expected scopes, but clicking "Authorize" fails with `POST /v1/oauth/{session_id}/authorize` returning 400 "Invalid request format" via a React Query mutation. The consent page stays on screen silently — no visible error to the user.

Root cause: Anthropic server-side enforcement blocking third-party OAuth consumers. Investigated in Q13-gate (2026-02-17): all gateway parameters were updated to match CC CLI v2.1.44 exactly (client ID, redirect URI, scopes including `user:mcp_servers`, PKCE S256 challenge). The failure persists in both normal and incognito browser sessions with all parameters matching. The OAuth session ID is a fixed server-side identifier for the registered application, not a client-side state issue. Anthropic has publicly stated they block third-party tools from using Claude Code OAuth tokens — this enforcement occurs at the authorization grant stage.

The `init-oauth` and `complete-oauth` endpoints remain functional code and will work if Anthropic lifts this restriction. No code changes are needed — only the server-side policy is blocking.

Account provisioning method: Use keychain extraction (see "Adding an Account — Keychain Extraction" above) to load tokens from an existing Claude Code installation. Extracted tokens work correctly for all proxy operations including token refresh.

### Credential File Missing `type` Field

The credential file requires a `type` field (value: `"oauth"`) on every credential entry. Omitting it causes a fatal parse error on startup (`missing field 'type'`), putting the pod into CrashLoopBackOff. If this happens, the PVC retains the bad file across restarts.

Recovery: scale the deployment to 0 (disable ArgoCD auto-sync first if enabled), run a temporary pod mounting the PVC, fix the file, then restore. See the field reference in `specs/anthropic-oauth-gateway.md` for the required format.
