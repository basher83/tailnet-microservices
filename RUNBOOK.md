# Anthropic OAuth Proxy — Operational Runbook

This runbook covers deployment, operation, monitoring, and troubleshooting of the anthropic-oauth-proxy service. The proxy supports two modes: passthrough (static header injection) and OAuth pool (PKCE auth, token refresh, subscription pooling).

## Architecture

The pod contains a single container. The Tailscale Operator manages tailnet connectivity via an Ingress resource that creates a proxy StatefulSet routing traffic from the tailnet to the Service ClusterIP.

```text
                    Tailnet
      +----------+         +---------------------+
      | Aperture | ------> | Tailscale Operator   |
      | (http://ai/)       | proxy (StatefulSet)  |
      +----------+         +----------+----------+
                                      |
                                      v
                           +---------------------+       +-----------+
                           | anthropic-oauth-proxy| ----> | Anthropic |
                           | (single container)   |      | API       |
                           +---------------------+       +-----------+
                            MagicDNS: anthropic-oauth-proxy
                            Proxy: 8080  |  Admin: 9090
```

In passthrough mode, the proxy injects the `anthropic-beta: oauth-2025-04-20` header and forwards to `https://api.anthropic.com`. In OAuth mode, it manages Bearer tokens from a pool of Claude Max subscriptions, handles automatic token refresh, and injects the full Anthropic header contract (anthropic-beta, anthropic-version, user-agent, system prompt). TLS termination for inbound traffic is handled by the tailnet WireGuard encryption. Outbound TLS to Anthropic uses `reqwest` with `rustls`.

## Deployment

### Initial Deploy

No secrets are required. The container image is public on GHCR (anonymous pull). Tailnet authentication is handled by the Tailscale Operator.

```bash
kubectl apply -k k8s/
```

This creates the namespace, ServiceAccount, ConfigMap, PVC, Deployment, Services (proxy + admin), and Ingress. The Tailscale Operator detects the Ingress and creates a StatefulSet to proxy from the tailnet to the ClusterIP.

### Verify Deployment

```bash
kubectl -n anthropic-oauth-proxy get pods
kubectl -n anthropic-oauth-proxy logs deployment/anthropic-oauth-proxy
```

A healthy startup sequence in the logs (JSON structured):

```text
{"message":"starting anthropic-oauth-proxy",...}
{"message":"loading configuration","path":"/etc/anthropic-oauth-proxy/config.toml",...}
{"message":"configuration loaded","listen_addr":"0.0.0.0:8080",...}
{"message":"state: Starting",...}
{"message":"state: Running — accepting requests","addr":"0.0.0.0:8080",...}
```

Verify the Tailscale Operator created its proxy StatefulSet:

```bash
kubectl -n anthropic-oauth-proxy get statefulset
```

### Switching to OAuth Mode

To switch from passthrough to OAuth mode, update the ConfigMap to uncomment the `[oauth]` and `[admin]` sections (and optionally remove `[[headers]]` — `[oauth]` takes precedence automatically). Then restart the deployment:

```bash
kubectl apply -k k8s/
kubectl -n anthropic-oauth-proxy rollout restart deployment/anthropic-oauth-proxy
```

The proxy starts in OAuth mode with an empty pool. Add accounts via the admin API (see below).

### Updating Configuration

The ConfigMap at `k8s/configmap.yaml` holds the TOML configuration. After editing:

```bash
kubectl apply -k k8s/
kubectl -n anthropic-oauth-proxy rollout restart deployment/anthropic-oauth-proxy
```

### Rollback

If a deployment introduces issues, roll back to the previous revision:

```bash
kubectl -n anthropic-oauth-proxy rollout undo deployment/anthropic-oauth-proxy
kubectl -n anthropic-oauth-proxy rollout status deployment/anthropic-oauth-proxy
```

Kubernetes retains the previous ReplicaSet by default, so `rollout undo` restores both the container image and the ConfigMap hash from the prior revision. For rollbacks beyond one revision, use `rollout undo --to-revision=<N>` where `N` is from `rollout history`.

## OAuth Account Management

Accounts are managed via the admin API on port 9090. The admin port is not exposed via Ingress — access it through `kubectl port-forward`.

### Accessing the Admin API

```bash
kubectl -n anthropic-oauth-proxy port-forward deployment/anthropic-oauth-proxy 9090:9090
```

All admin commands below assume port-forwarding is active.

### Adding an Account (PKCE Flow)

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

### Listing Accounts

```bash
curl -s http://localhost:9090/admin/accounts | jq .
```

Response includes account IDs and status (available, cooling_down, disabled). Tokens are never exposed.

### Removing an Account

```bash
curl -s -X DELETE http://localhost:9090/admin/accounts/claude-max-1739059200 | jq .
```

Removes the account from the pool and credential store. Idempotent.

### Pool Status

```bash
curl -s http://localhost:9090/admin/pool | jq .
```

Returns per-account status, cooldown timers, and overall pool health.

### Credential Persistence

OAuth credentials are stored in `/data/credentials.json` on a PersistentVolumeClaim. Pod restarts preserve tokens — no need to re-authenticate accounts after restart.

The single-replica constraint exists because PKCE state is held in-memory. Running multiple pods would split the init/complete flow across pods. This does not affect credential persistence (PVC survives pod restarts).

## Endpoints

| Path | Port | Purpose | Response |
|------|------|---------|----------|
| `GET /health` | 8080 | Startup, liveness, readiness probe | JSON with status, uptime, pool status |
| `GET /metrics` | 8080 | Prometheus scrape target | Text exposition format |
| `* /*` | 8080 | Proxy fallback | Forwards to upstream |
| `GET /admin/accounts` | 9090 | List accounts | JSON account list |
| `POST /admin/accounts/init-oauth` | 9090 | Start PKCE flow | JSON with auth URL |
| `POST /admin/accounts/complete-oauth` | 9090 | Exchange code | JSON confirmation |
| `DELETE /admin/accounts/{id}` | 9090 | Remove account | JSON confirmation |
| `GET /admin/pool` | 9090 | Pool health summary | JSON pool status |

### Health Endpoint Response

The health endpoint always returns HTTP 200 when the listener is bound. The `status` field indicates pool health.

Passthrough mode:

```json
{
  "status": "healthy",
  "mode": "passthrough",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}
```

OAuth mode:

```json
{
  "status": "degraded",
  "mode": "anthropic",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0,
  "pool": {
    "accounts_total": 3,
    "accounts_available": 2,
    "accounts_cooling": 1,
    "accounts_disabled": 0,
    "accounts": [
      { "id": "claude-max-1", "status": "available" },
      { "id": "claude-max-2", "status": "cooling_down", "cooldown_remaining_secs": 3600 },
      { "id": "claude-max-3", "status": "available" }
    ]
  }
}
```

Status mapping: all available = `healthy`, some cooling/disabled = `degraded`, all cooling/disabled = `unhealthy`.

## Monitoring

### Prometheus Metrics

Scrape `GET /metrics` on port 8080. Metrics emitted:

`proxy_requests_total` (counter) with labels `status` and `method` tracks completed proxy requests. Use this for request rate and error rate calculations.

`proxy_request_duration_seconds` (histogram) with label `status` and bucket boundaries from 5ms to 60s. Use `histogram_quantile()` in PromQL to compute latency percentiles (p50, p90, p99) from the histogram buckets at query time.

`proxy_upstream_errors_total` (counter) with label `error_type` tracks upstream failures. Error types: `timeout` (upstream did not respond within `timeout_secs`), `connection` (TCP connection to upstream failed), `invalid_request` (request body exceeded 10 MiB limit or malformed request).

OAuth mode adds four additional metrics:

`pool_account_status` (gauge) with labels `account_id` and `status`. Tracks the current state of each account in the pool (available, cooling_down, disabled).

`pool_failovers_total` (counter) with labels `from_account` and `reason`. Incremented when the proxy fails over from one account to the next due to quota exhaustion or permanent error.

`pool_token_refreshes_total` (counter) with labels `account_id` and `result`. Tracks token refresh attempts (success or failure).

`pool_quota_exhaustions_total` (counter) with label `account_id`. Incremented when an account hits its usage quota (429 with quota message).

### Key Alerts

Alert on sustained upstream errors:

```text
rate(proxy_upstream_errors_total[5m]) > 0.1
```

Alert on p99 latency approaching the 60s timeout. The `sum by (le)` aggregation is required because the histogram carries a `status` label:

```text
histogram_quantile(0.99, sum by (le) (rate(proxy_request_duration_seconds_bucket[5m]))) > 30
```

Alert when all pool accounts are exhausted (OAuth mode). This fires when no accounts are available:

```text
sum(pool_account_status{status="available"}) == 0
```

Alert on high failover rate indicating quota pressure across accounts:

```text
rate(pool_failovers_total[5m]) > 0.05
```

### Token Refresh Troubleshooting

If `pool_token_refreshes_total{result="failure"}` is incrementing, accounts are failing to refresh their OAuth tokens. Common causes:

The refresh token itself has expired or been revoked. The account must be removed and re-added via the admin API PKCE flow.

The Anthropic token endpoint (`https://console.anthropic.com/v1/oauth/token`) is unreachable. Check outbound network connectivity from the pod. Transient failures are retried on the next refresh cycle (default: every 5 minutes).

An account marked `disabled` in the pool health indicates its refresh token is permanently invalid. Remove it and re-authenticate.

### Structured Logs

All log output is JSON. Key fields to filter on:

- `message`: human-readable event description
- `request_id`: `req_<uuid>` correlating a proxy request through its lifecycle
- `error`: error message when something fails

Set log verbosity via the `LOG_LEVEL` environment variable in the deployment. Accepts standard tracing directives: `error`, `warn`, `info`, `debug`, `trace`. Defaults to `info`.

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

Upstream did not respond within the configured `timeout_secs` (default: 60s). The proxy automatically retries timeouts up to 2 times (3 total attempts) with 100ms backoff between attempts. If all attempts time out, it returns 504.

For sustained 504s, check Anthropic API status. If the API is healthy, consider increasing `timeout_secs` in the ConfigMap for long-running requests.

### Proxy Returning 400 Bad Request

Either the request body exceeds the 10 MiB hardcoded limit, or the request is malformed. Check the `request_id` in the error response JSON and correlate with proxy logs.

### Proxy Returning 429 (OAuth Mode)

In OAuth mode, the proxy attempts failover to the next available account when the current account's quota is exhausted (429 with quota message). If all accounts are exhausted, the proxy returns 429 to the client.

Check pool status via the health endpoint or admin API to see which accounts are cooling down and when they will become available again. Default cooldown is 2 hours (configurable via `cooldown_secs`).

### Pool Exhausted (OAuth Mode)

When all accounts are in `cooling_down` or `disabled` state, the proxy returns 429 to all requests. To resolve:

- Wait for cooldown timers to expire (check `cooldown_remaining_secs` in pool health)
- Add more accounts via the admin API PKCE flow
- Remove and re-add disabled accounts (disabled means refresh token is permanently invalid)

### High Latency

Check `proxy_request_duration_seconds` histogram percentiles. Latency is dominated by upstream response time. The proxy adds negligible overhead (header injection, hop-by-hop stripping, JSON body modification in OAuth mode).

If latency correlates with high concurrency, check if `max_connections` (default: 1000) is being hit. The concurrency limiter queues excess requests rather than rejecting them, which manifests as increased latency rather than errors. Health and metrics endpoints are outside the concurrency limit and remain responsive regardless of proxy load.

## Graceful Shutdown

On SIGTERM (Kubernetes pod termination), the proxy stops accepting new connections and waits for in-flight requests to complete. The `in_flight` atomic counter tracks active requests. The proxy enforces a 5-second `DRAIN_TIMEOUT` starting from when it receives the signal. If in-flight requests complete within 5 seconds, shutdown is clean. If not, the proxy force-exits after 5 seconds regardless of the Kubernetes `terminationGracePeriodSeconds`.

The shutdown sequence logged:

```text
{"message":"received SIGTERM, shutting down",...}
{"message":"all in-flight requests drained",...}
{"message":"shutdown complete",...}
```

If requests are still in flight when the 5-second drain timeout expires:

```text
{"message":"drain timeout exceeded, forcing shutdown","remaining":3,"drain_timeout_secs":5,...}
```

## Resource Limits

Default resource configuration from `k8s/deployment.yaml`:

| Container | CPU request | CPU limit | Memory request | Memory limit |
|-----------|-------------|-----------|----------------|--------------|
| proxy | 50m | 500m | 32Mi | 128Mi |

The proxy binary is approximately 5MB and has minimal memory overhead. Increase memory limits if serving large request/response bodies concurrently, though the 10 MiB body size limit provides a natural ceiling.

## Header Discovery Maintenance

When the Claude CLI updates, the required headers may change. To discover the current header contract:

```bash
# Install the updated Claude CLI, then sniff traffic
mitmdump --set flow_detail=4 -p 8888
HTTPS_PROXY=http://127.0.0.1:8888 claude --print "hello"
```

Compare the captured headers against the constants in `services/oauth-proxy/src/provider_impl.rs`. Update the constants and run tests if anything has changed.
