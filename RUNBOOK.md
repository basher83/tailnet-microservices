# Anthropic OAuth Proxy — Operational Runbook

This runbook covers deployment, operation, monitoring, and troubleshooting of the anthropic-oauth-proxy service running as a Kubernetes pod with a tailscaled sidecar.

## Architecture

The pod contains two containers sharing a Unix socket volume:

```text
                           Pod
              +-----------+----------+
              |  proxy    | tailscaled|
              |  (Rust)   | (sidecar) |
              |           |           |
              | port 8080 | userspace |
              +-----+-----+-----+----+
                    |           |
                    +-----------+
                   /var/run/tailscale/
                   tailscaled.sock
```

The proxy queries tailscaled via the Unix socket for tailnet identity (hostname, IP), then listens on port 8080 and proxies all non-health/metrics requests to `https://api.anthropic.com`, injecting the `anthropic-beta: oauth-2025-04-20` header. TLS termination is handled by the tailnet WireGuard encryption, not the proxy itself.

## Deployment

### Prerequisites

A Tailscale auth key with appropriate ACL permissions. Generate one from the Tailscale admin console. Reusable, ephemeral keys are recommended for Kubernetes deployments.

### Initial Deploy

Create the namespace and secret first, then apply manifests:

```bash
kubectl create namespace anthropic-oauth-proxy

kubectl create secret generic tailscale-authkey \
  --namespace=anthropic-oauth-proxy \
  --from-literal=TS_AUTHKEY=tskey-auth-XXXXX

kubectl apply -k k8s/
```

### Verify Deployment

```bash
kubectl -n anthropic-oauth-proxy get pods
kubectl -n anthropic-oauth-proxy logs -c proxy <pod-name>
kubectl -n anthropic-oauth-proxy logs -c tailscaled <pod-name>
```

A healthy startup sequence in the proxy container logs (JSON structured):

```text
{"message":"starting anthropic-oauth-proxy",...}
{"message":"loading configuration","path":"/etc/anthropic-oauth-proxy/config.toml",...}
{"message":"configuration loaded","listen_addr":"0.0.0.0:8080",...}
{"message":"state: ConnectingTailnet",...}
{"message":"state: Starting",...}
{"message":"state: Running — accepting requests","addr":"0.0.0.0:8080",...}
```

### Updating Configuration

The ConfigMap at `k8s/configmap.yaml` holds the TOML configuration. After editing:

```bash
kubectl apply -k k8s/
kubectl -n anthropic-oauth-proxy rollout restart deployment/anthropic-oauth-proxy
```

### Rotating the Tailscale Auth Key

```bash
kubectl -n anthropic-oauth-proxy delete secret tailscale-authkey

kubectl create secret generic tailscale-authkey \
  --namespace=anthropic-oauth-proxy \
  --from-literal=TS_AUTHKEY=tskey-auth-NEWKEY

kubectl -n anthropic-oauth-proxy rollout restart deployment/anthropic-oauth-proxy
```

## Endpoints

| Path | Purpose | Response |
|------|---------|----------|
| `GET /health` | Liveness and readiness probe | JSON with status, tailnet state, uptime, request count |
| `GET /metrics` | Prometheus scrape target | Text exposition format |
| `* /*` | Proxy fallback | Forwards to upstream with header injection |

### Health Endpoint Response

When connected to tailnet:

```json
{
  "status": "healthy",
  "tailnet": "connected",
  "tailnet_hostname": "anthropic-oauth-proxy",
  "tailnet_ip": "100.x.y.z",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}
```

When tailnet is not connected (returns 503 Service Unavailable, should not happen in steady state):

```json
{
  "status": "degraded",
  "tailnet": "not_connected",
  "uptime_seconds": 0,
  "requests_served": 0,
  "errors_total": 0
}
```

## Monitoring

### Prometheus Metrics

Scrape `GET /metrics` on port 8080. Four metrics are emitted:

`proxy_requests_total` (counter) with labels `status` and `method` tracks completed proxy requests. Use this for request rate and error rate calculations.

`proxy_request_duration_seconds` (histogram) with label `status` provides latency percentiles. The histogram automatically computes p50, p90, p99, and p999 quantiles.

`proxy_upstream_errors_total` (counter) with label `error_type` tracks upstream failures. Error types include `timeout` (upstream did not respond within `timeout_secs`), `connection` (TCP connection to upstream failed), `invalid_request` (request body exceeded 10MB limit or malformed request), `response_read` (failed to read upstream response body), and `internal` (unexpected proxy error).

`tailnet_connected` (gauge) is 1 when the proxy has an active tailnet connection and 0 otherwise. Alert if this drops to 0 during normal operation.

### Key Alerts to Configure

Alert on `tailnet_connected == 0` for more than 60 seconds. The proxy cannot serve traffic without a tailnet identity.

Alert on `rate(proxy_upstream_errors_total[5m]) > 0.1` to catch sustained upstream failures.

Alert on `histogram_quantile(0.99, rate(proxy_request_duration_seconds_bucket[5m])) > 30` to detect upstream latency degradation approaching the 60s timeout.

### Structured Logs

All log output is JSON. Key fields to filter on:

- `message`: human-readable event description
- `request_id`: `req_<uuid>` correlating a proxy request through its lifecycle
- `error`: error message when something fails
- `retry_in_secs`: seconds until next retry (tailnet connection failures)

Set log verbosity via the `LOG_LEVEL` environment variable in the deployment. Accepts standard tracing directives: `error`, `warn`, `info`, `debug`, `trace`. Defaults to `info`.

## Troubleshooting

### Pod CrashLoopBackOff

Check proxy container logs first. The three lifecycle errors that cause crashes:

`TailnetAuth` ("Tailnet authentication failed") means the `TS_AUTHKEY` secret is invalid or expired. Generate a new auth key from the Tailscale admin console and rotate the secret (see Rotating the Tailscale Auth Key above).

`TailnetMachineAuth` ("Tailnet needs machine authorization") means the node requires admin approval in the Tailscale admin console. Navigate to the Tailscale admin console, find the pending node, and approve it. Then restart the pod.

`TailnetNotRunning` ("Tailnet daemon not running") means the tailscaled sidecar is not running or the Unix socket at `/var/run/tailscale/tailscaled.sock` is not reachable. Check the tailscaled container logs. Verify both containers share the `tailscale-socket` volume.

`TailnetConnect` ("Tailnet connection failed") is a transient error. The proxy retries up to 5 times with exponential backoff (1s, 2s, 4s, 8s, 16s). If all 5 retries fail, the process exits with code 1 and Kubernetes restarts it. Check network connectivity to the Tailscale coordination server.

### Proxy Returning 502 Bad Gateway

The upstream at `https://api.anthropic.com` is unreachable or returning connection errors. Check:

```bash
kubectl -n anthropic-oauth-proxy exec -c proxy <pod-name> -- \
  curl -s -o /dev/null -w '%{http_code}' https://api.anthropic.com/v1/messages
```

If DNS fails, check that the runtime image has `ca-certificates` installed (it does in the default Dockerfile) and that the pod has outbound internet access.

### Proxy Returning 504 Gateway Timeout

Upstream did not respond within the configured `timeout_secs` (default: 60s). The proxy automatically retries timeouts up to 2 times (3 total attempts) with 100ms backoff between attempts. If all attempts time out, it returns 504.

For sustained 504s, check Anthropic API status. If the API is healthy, consider increasing `timeout_secs` in the ConfigMap for long-running requests.

### Proxy Returning 400 Bad Request

Either the request body exceeds the 10MB hardcoded limit, or the request is malformed. Check the `request_id` in the error response JSON and correlate with proxy logs.

### High Latency

Check `proxy_request_duration_seconds` histogram percentiles. Latency is dominated by upstream response time. The proxy adds negligible overhead (header injection, hop-by-hop stripping).

If latency correlates with high concurrency, check if `max_connections` (default: 1000) is being hit. The concurrency limiter queues excess requests rather than rejecting them, which manifests as increased latency rather than errors.

### Tailscaled Sidecar Issues

The tailscaled container runs in userspace mode (`TS_USERSPACE=true`) to avoid requiring `NET_ADMIN` capabilities. Common issues:

`TS_AUTHKEY` expired or revoked: the sidecar will fail to authenticate. Rotate the secret.

State directory corruption: the sidecar stores state in `/var/lib/tailscale` (an `emptyDir` volume). Deleting the pod clears this state and forces re-authentication.

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
| tailscaled | 50m | 250m | 64Mi | 256Mi |

The proxy binary is approximately 5MB and has minimal memory overhead. Increase memory limits if serving large request/response bodies concurrently, though the 10MB body size limit provides a natural ceiling.
