# Monitoring & Endpoints

> **Operational Runbook** · [Index](./README.md) · [Deployment](./deployment.md) · [Accounts](./accounts.md) · [Monitoring](./monitoring.md) · [Troubleshooting](./troubleshooting.md) · [Clients](./clients.md) · [Header Parity](./header-parity.md)

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
    "accounts_cooling_down": 1,
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

`pool_failovers_total` (counter) with labels `from_account` and `reason`. Incremented when the proxy fails over from one account to the next due to quota exhaustion.

`pool_token_refreshes_total` (counter) with labels `account_id` and `result`. Tracks token refresh attempts (success or failure).

`pool_quota_exhaustions_total` (counter) with label `account_id`. Incremented when an account hits its usage quota (429 with quota message).

### Key Alerts

Alert on sustained upstream errors:

```text
rate(proxy_upstream_errors_total[5m]) > 0.1
```

Alert on p99 latency approaching the configured timeout. The production K8s config currently sets `timeout_secs = 180`. The `sum by (le)` aggregation is required because the histogram carries a `status` label:

```text
histogram_quantile(0.99, sum by (le) (rate(proxy_request_duration_seconds_bucket[5m]))) > 30
```

Alert when all pool accounts are exhausted (OAuth mode). This fires when no accounts are available:

```text
sum(pool_account_status{status="available"}) == 0
```

Alert on any disabled account (OAuth mode). This is the refresh-token-expired case: it does not self-heal and, with a single-account pool, it *is* the outage. Fire immediately, not after a for-duration:

```text
sum(pool_account_status{status="disabled"}) > 0
```

Alert on high failover rate indicating quota pressure across accounts:

```text
rate(pool_failovers_total[5m]) > 0.05
```

### Token Refresh Troubleshooting

If `pool_token_refreshes_total{result="failure"}` is incrementing, accounts are failing to refresh their OAuth tokens. Common causes:

The refresh token itself has expired or been revoked — the token endpoint returns an OAuth `invalid_grant` (which Anthropic sends as HTTP 400), or a 401/403. This is permanent: the account is marked `disabled` by whichever path hit it (request-time inline refresh or the background task). Remove the account and load fresh credentials using keychain extraction. The admin API PKCE flow is in the code but currently fails on request-shape bugs (`code=true`, `state` format) — see Troubleshooting → Known Issues; it is not policy-blocked.

The Anthropic token endpoint (`https://platform.claude.com/v1/oauth/token`) is unreachable, or returned a 5xx/429. Check outbound network connectivity from the pod. Transient failures do **not** disable the account — it stays `available` and is retried on the next request and on the next refresh cycle (default: every 5 minutes).

An account marked `disabled` in the pool health indicates its refresh token is permanently invalid. Remove it and re-authenticate. Refresh tokens have been observed to expire ~6 weeks after keychain extraction (`error_description: "Refresh token expired"`); see [Accounts → Refresh Token Lifetime](./accounts.md#refresh-token-lifetime-and-re-auth) for observed dates and the re-auth cadence to plan for.

Log lines to grep for:

| `message` | Level | Meaning |
|---|---|---|
| `background token refresh succeeded` | INFO | Normal; expect one every ~7h45m per account |
| `refresh token rejected, disabling account` | WARN | Permanent failure; the **first** occurrence is the outage start. It repeats every 5 min for an already-disabled account (known issue) — do not read the latest one as "it just happened" |
| `background refresh failed (transient), will retry next cycle` | WARN | Transient (network/5xx/429); account stays `available` |
| `inline refresh rejected (permanent), disabling account` / `inline refresh failed (transient), skipping account this round` | WARN | Request-path refresh; same permanent/transient split |
| `provider prepare_request failed` … `pool_exhausted` | ERROR | A client got a 503; one line per request |

Note `/health` returns HTTP 200 whenever the listener is up, including with every account disabled — alert on `pool_account_status`, or on `.status != "healthy"` in the JSON body, never on the HTTP code alone.

### Structured Logs

All log output is JSON. Key fields to filter on:

- `message`: human-readable event description
- `request_id`: `req_<uuid>` correlating a proxy request through its lifecycle
- `error`: error message when something fails

Set log verbosity via the `LOG_LEVEL` environment variable in the deployment. Accepts standard tracing directives: `error`, `warn`, `info`, `debug`, `trace`. Defaults to `info`.

