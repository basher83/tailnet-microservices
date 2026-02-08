# Anthropic OAuth Proxy — Operational Runbook

This runbook covers deployment, operation, monitoring, and troubleshooting of the anthropic-oauth-proxy service running as a single-container Kubernetes pod. Tailnet exposure is delegated to the Tailscale Operator via Service annotations.

## Architecture

The pod contains a single container. The Tailscale Operator manages tailnet connectivity externally via a StatefulSet it creates from the annotated Service.

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
```

The proxy listens on port 8080 and forwards all non-health/metrics requests to `https://api.anthropic.com`, injecting the `anthropic-beta: oauth-2025-04-20` header. TLS termination for inbound traffic is handled by the tailnet WireGuard encryption, not the proxy itself. Outbound TLS to Anthropic uses `reqwest` with `rustls`.

## Deployment

### Initial Deploy

No secrets are required. The container image is public on GHCR (anonymous pull). Tailnet authentication is handled by the Tailscale Operator.

```bash
kubectl apply -k k8s/
```

This creates the namespace, ServiceAccount, ConfigMap, Deployment, and Service. The Tailscale Operator detects the `tailscale.com/expose: "true"` annotation on the Service and creates a StatefulSet to proxy from the tailnet to the ClusterIP.

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

## Endpoints

| Path | Purpose | Response |
|------|---------|----------|
| `GET /health` | Startup, liveness, and readiness probe | JSON with status, uptime, request count |
| `GET /metrics` | Prometheus scrape target | Text exposition format |
| `* /*` | Proxy fallback | Forwards to upstream with header injection |

### Health Endpoint Response

The health endpoint always returns 200 when the HTTP listener is bound. There is no degraded state.

```json
{
  "status": "healthy",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}
```

## Monitoring

### Prometheus Metrics

Scrape `GET /metrics` on port 8080. Three metrics are emitted:

`proxy_requests_total` (counter) with labels `status` and `method` tracks completed proxy requests. Use this for request rate and error rate calculations.

`proxy_request_duration_seconds` (histogram) with label `status` and bucket boundaries from 5ms to 60s. Use `histogram_quantile()` in PromQL to compute latency percentiles (p50, p90, p99) from the histogram buckets at query time.

`proxy_upstream_errors_total` (counter) with label `error_type` tracks upstream failures. Error types: `timeout` (upstream did not respond within `timeout_secs`), `connection` (TCP connection to upstream failed), `invalid_request` (request body exceeded 10 MiB limit or malformed request).

### Key Alerts to Configure

Alert on `rate(proxy_upstream_errors_total[5m]) > 0.1` to catch sustained upstream failures.

Alert on `histogram_quantile(0.99, sum by (le) (rate(proxy_request_duration_seconds_bucket[5m]))) > 30` to detect upstream latency degradation approaching the 60s timeout. The `sum by (le)` aggregation is required because the histogram carries a `status` label — without it, `histogram_quantile` receives multiple series per `le` bucket and produces incorrect results.

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

If the proxy pod is running but not reachable via MagicDNS (`anthropic-oauth-proxy`), the issue is with the Tailscale Operator, not the proxy. Check that the Operator created its proxy StatefulSet and that it is healthy:

```bash
kubectl -n anthropic-oauth-proxy get statefulset
kubectl -n anthropic-oauth-proxy get pods -l app=tailscale
```

Verify the Service annotations are correct:

```bash
kubectl -n anthropic-oauth-proxy get svc anthropic-oauth-proxy -o yaml | grep tailscale
```

Expected annotations: `tailscale.com/expose: "true"` and `tailscale.com/hostname: "anthropic-oauth-proxy"`.

### Proxy Returning 502 Bad Gateway

The upstream at `https://api.anthropic.com` is unreachable or returning connection errors. The proxy container is a minimal image without `curl`, so use port-forwarding to test from your workstation:

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

### High Latency

Check `proxy_request_duration_seconds` histogram percentiles. Latency is dominated by upstream response time. The proxy adds negligible overhead (header injection, hop-by-hop stripping).

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
