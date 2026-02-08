# Spec: Anthropic OAuth Proxy

**Status:** Complete
**Created:** 2026-02-05
**Updated:** 2026-02-08
**Author:** Astrogator + Brent

---

## Overview

Single-binary Rust service that injects OAuth headers and proxies requests to api.anthropic.com. Tailnet exposure is delegated to the Tailscale Operator via a Tailscale Ingress resource. The proxy runs as a single-container pod with zero secrets.

**Design Principles:**

| Principle | Implementation |
|-----------|----------------|
| Single responsibility | One binary, one function |
| Minimal external deps | Static binary, no sidecar, no secrets |
| Tailnet-native | MagicDNS identity via Tailscale Operator |
| Pure state machines | Event → Action, caller handles I/O |
| Type-safe | Rust compiler catches spec deviations |

**Source files:**

- `services/oauth-proxy/src/main.rs` — Entry point, CLI parsing, axum server
- `services/oauth-proxy/src/config.rs` — Configuration types and loading
- `services/oauth-proxy/src/service.rs` — Service state machine
- `services/oauth-proxy/src/proxy.rs` — HTTP proxy logic
- `services/oauth-proxy/src/metrics.rs` — Prometheus metrics exposition
- `crates/common/src/error.rs` — Common error types
- `crates/common/src/lib.rs` — Re-exports

---

## First Target: Anthropic OAuth Proxy

### Problem
Aperture lacks custom header injection. Claude Max OAuth tokens require the `anthropic-beta: oauth-2025-04-20` header.

### Solution
Rust binary that injects required headers and proxies to Anthropic API. Tailnet exposure is handled externally by the Tailscale Operator.

---

## Architecture

```text
┌──────────────────────────────────────────────────────────────────┐
│                           Tailnet                                │
│                                                                  │
│  ┌─────────┐      ┌──────────────────┐                           │
│  │ Aperture│ ───► │  Tailscale       │                           │
│  │ (http://ai/)   │  Operator proxy  │                           │
│  └─────────┘      └────────┬─────────┘                           │
│                            │                                     │
│                            ▼                                     │
│                    ┌──────────────────────────┐      ┌─────────┐ │
│                    │  anthropic-oauth-proxy    │ ───► │External │ │
│                    │  (single container)       │      │Anthropic│ │
│                    └──────────────────────────┘      │   API   │ │
│                                                      └─────────┘ │
│                    MagicDNS: anthropic-oauth-proxy                │
└──────────────────────────────────────────────────────────────────┘
```

Tailnet exposure is provided by the Tailscale Operator via a Tailscale Ingress (`ingressClassName: tailscale`). The Ingress routes tailnet HTTP traffic to the ClusterIP Service. The MagicDNS hostname `anthropic-oauth-proxy` is derived from `tls[0].hosts[0]`. The Rust binary has no tailnet code.

---

## Types

### Core Types

| Type | Description | Fields |
|------|-------------|--------|
| `Config` | Service configuration | `proxy: ProxyConfig`, `headers: Vec<HeaderInjection>` |
| `ProxyConfig` | HTTP proxy settings | `listen_addr: SocketAddr`, `upstream_url: String`, `timeout_secs: u64`, `max_connections: usize` |
| `HeaderInjection` | Header to inject | `name: String`, `value: String` (not sensitive; e.g. `anthropic-beta` value) |
| `ServiceMetrics` | Runtime metrics | `requests_total: Arc<AtomicU64>`, `errors_total: Arc<AtomicU64>`, `in_flight: Arc<AtomicU64>`, `started_at: Instant` |

---

## State Machine

The service uses an explicit state machine for lifecycle management.

### `ServiceState` Enum

| State | Description | Fields |
|-------|-------------|--------|
| `Initializing` | Loading config, setting up resources | (no data) |
| `Starting` | Starting HTTP listener | `listen_addr: SocketAddr` |
| `Running` | Accepting and proxying requests | `listen_addr: SocketAddr` |
| `Draining` | Graceful shutdown, finishing in-flight | `deadline: Instant` (drain coordination handled by axum's graceful shutdown and the `in_flight` atomic counter) |
| `Stopped` | Terminal state | `exit_code: i32` |

### `ServiceEvent` Enum

| Event | Description | Payload |
|-------|-------------|---------|
| `ConfigLoaded` | Configuration parsed successfully | `listen_addr: SocketAddr` |
| `ListenerReady` | HTTP listener bound | (no data) |
| `RequestReceived` | Incoming HTTP request | `request_id: String` (request object handled directly by proxy handler) |
| `RequestCompleted` | Request finished (success or error) | `request_id: String`, `duration: Duration`, `error: Option<String>` |
| `ShutdownSignal` | SIGTERM/SIGINT received | — |
| `DrainTimeout` | Drain deadline exceeded | — |

### `ServiceAction` Enum

| Action | Description | Payload |
|--------|-------------|---------|
| `StartListener` | Bind HTTP listener | `addr: SocketAddr` |
| `Shutdown` | Exit process | `exit_code: i32` |
| `None` | No-op | — |

Config loading, request proxying, response sending, and metric emission happen outside the state machine. `LoadConfig` occurs before the state machine starts. `ProxyRequest`/`SendResponse` are handled directly by the axum proxy handler. `EmitMetric` calls are inlined at the call site.

The state machine drives the startup lifecycle (`Initializing` through `Running`). Once `Running`, graceful shutdown is handled by axum's `with_graceful_shutdown` mechanism with a `DRAIN_TIMEOUT` enforcement, rather than by firing `ShutdownSignal`/`DrainTimeout` events through the state machine. The `Draining` and `Stopped` transitions are implemented and tested for correctness but are not exercised at runtime.

`RequestReceived` and `RequestCompleted` events exist in the enum for completeness but are handled directly by the proxy handler via atomic counters, not through the state machine. Calling `handle_event` with these events from `Running` state returns a defensive no-op (`Running`, `None`) — the state machine stays in `Running` with no action.

---

## State Transitions

| Current State | Event | New State | Action |
|---------------|-------|-----------|--------|
| `Initializing` | `ConfigLoaded(cfg)` | `Starting` | `StartListener(addr)` |
| `Starting` | `ListenerReady` | `Running` | `None` |
| `Running` | `RequestReceived`/`RequestCompleted` | `Running` | `None` (defensive no-op; handled by proxy handler) |
| `Running` | `ShutdownSignal` | `Draining` | `None` |
| `Draining` | `DrainTimeout` | `Stopped` | `Shutdown(0)` |
| `Draining` | `ShutdownSignal` | `Stopped` | `Shutdown(0)` |
| `Stopped` | *any* | `Stopped` | `None` (terminal, inert) |
| *any non-Running* | `ShutdownSignal` | `Stopped` | `Shutdown(0)` |

---

## HTTP Proxy Protocol

### Request Flow

```text
Client Request
      │
      ▼
┌─────────────────┐
│ Parse Request   │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Inject Headers  │◄── anthropic-beta: oauth-2025-04-20
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Forward to      │───► api.anthropic.com
│ Upstream        │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Return Response │
└─────────────────┘
```

### Header Injection Rules

| Condition | Action |
|-----------|--------|
| Header not present | Add header |
| Header present | Replace value |
| `authorization` header | Pass through unchanged (protected from injection) |
| `host` header | Strip before forwarding (reqwest derives correct host from upstream URL) |
| Hop-by-hop headers | Strip before forwarding |

### Hop-by-hop Headers (strip from both request and response)

```text
connection, keep-alive, proxy-authenticate, proxy-authorization,
te, trailer, transfer-encoding, upgrade
```

### Body Size Limit

Requests with bodies exceeding 10 MiB (10,485,760 bytes) are rejected with 400 Bad Request.

### Response Streaming

Response bodies are streamed from upstream to the client without buffering. This is critical for SSE (Server-Sent Events) where the Anthropic API streams Claude's responses in real-time. The proxy collects the upstream status code and headers before streaming begins, enabling metrics recording for all responses including long-lived SSE connections. Mid-stream errors result in connection closure (HTTP status already sent); SSE clients handle reconnection automatically.

---

## Error Handling

### Common error types (`common::Error`)

The `common` crate defines errors used during startup (config loading). These cause process exit before the state machine reaches `Running`:

| Variant | Description |
|---------|-------------|
| `Config(String)` | Configuration validation failure |
| `Io(io::Error)` | File system errors (config file not found, etc.) |
| `Toml(toml::de::Error)` | TOML parse errors |

### Errors handled inline

These errors are handled directly in `proxy.rs` and `main.rs` rather than through a centralized error type:

| Error | Handled by | HTTP Status |
|-------|-----------|-------------|
| `ConfigError` | `common::Error` in `Config::load()`, exits before state machine starts | N/A (process exits) |
| `ListenerBindError` | `anyhow::Context` on `TcpListener::bind()`, exits before serving | N/A (process exits) |
| `UpstreamTimeout` | `proxy.rs` retry loop, returns HTTP response directly | 504 Gateway Timeout |
| `UpstreamError` (non-2xx) | `proxy.rs` passes upstream response through unchanged | Upstream status code |
| `UpstreamError` (connection failure) | `proxy.rs` returns error response directly | 502 Bad Gateway |
| `InvalidRequest` (body too large) | `axum::body::to_bytes()` limit in `proxy.rs` | 400 Bad Request |

### Retry Strategy

| Error Type | Max Retries | Backoff |
|------------|-------------|---------|
| `UpstreamTimeout` | 2 (3 total attempts) | Fixed: 100ms |

### Error Response Format

```json
{
  "error": {
    "type": "proxy_error",
    "message": "Upstream timeout after 60s (3 attempts)",
    "request_id": "req_abc123"
  }
}
```

---

## Configuration

### File Format (TOML)

```toml
# anthropic-oauth-proxy.toml

[proxy]
listen_addr = "0.0.0.0:8080"
upstream_url = "https://api.anthropic.com"
timeout_secs = 60
max_connections = 1000

[[headers]]
name = "anthropic-beta"
value = "oauth-2025-04-20"
```

### Environment Variables

| Variable | Description | Precedence |
|----------|-------------|------------|
| `CONFIG_PATH` | Config file path | Fallback when CLI `--config` is not provided |
| `LOG_LEVEL` | Logging verbosity | Checked first; falls back to `RUST_LOG` |

### Precedence

```text
CLI args > Environment vars > Config file > Defaults
```

### Defaults

| Field | Default |
|-------|---------|
| `timeout_secs` | 60 |
| `max_connections` | 1000 |
| `headers` | `[]` (empty) |

---

## Observability

### Structured Logging

```rust
use tracing::{info, warn, error, instrument};

#[instrument(skip_all, fields(request_id = %request_id, method = %request.method(), path = %request.uri().path()))]
async fn proxy_request(state: &ProxyState, request: Request, request_id: String) {
    info!("received request");
    // ...
    info!(latency_ms = elapsed.as_millis() as u64, "request completed");
}
```

Logging is initialized with JSON output via `tracing-subscriber`. The `LOG_LEVEL` env var is checked first, falling back to `RUST_LOG`, with a final default of `info`.

### Metrics (Prometheus)

| Metric | Type | Labels |
|--------|------|--------|
| `proxy_requests_total` | Counter | `status`, `method` |
| `proxy_request_duration_seconds` | Histogram | `status` |
| `proxy_upstream_errors_total` | Counter | `error_type` |

Histogram buckets for `proxy_request_duration_seconds`: 5ms, 10ms, 25ms, 50ms, 100ms, 250ms, 500ms, 1s, 2.5s, 5s, 10s, 30s, 60s.

Metrics are served on `GET /metrics` in Prometheus text exposition format. The metrics endpoint is outside the concurrency limit so Prometheus scrapes are never blocked.

### Health Endpoint

```text
GET /health

200 OK
{
  "status": "healthy",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}
```

The health endpoint always returns 200 when the HTTP listener is bound. There is no degraded state. The health endpoint is outside the concurrency limit so Kubernetes probes are never blocked.

---

## Build & Distribution

### Cargo.toml

Package name is `oauth-proxy` with binary name `anthropic-oauth-proxy` (via `[[bin]]` table). Dependencies are managed via workspace `Cargo.toml`:

```toml
# Key workspace dependencies
tokio = { version = "1", features = ["full"] }
axum = "0.8"
reqwest = { version = "0.13", default-features = false, features = ["rustls", "http2", "stream"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.9"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
metrics = "0.24"
metrics-exporter-prometheus = "0.18"
tower = { version = "0.5", features = ["limit"] }
uuid = { version = "1", features = ["v4"] }
thiserror = "2"
anyhow = "1"
```

### Build Targets

| Target | Binary Size | Notes |
|--------|-------------|-------|
| `aarch64-apple-darwin` | ~4.4MB | Primary (Mac Mini) |
| `x86_64-unknown-linux-gnu` | ~5.4MB | K8s deployment |
| `aarch64-unknown-linux-gnu` | ~4.7MB | Pi / ARM servers |

### Release Profile

```toml
[profile.release]
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

### Container Image

Multi-stage Dockerfile: `rust:1-bookworm` builder, `debian:bookworm-slim` runtime. The builder uses cargo cache mounts for registry and target directory. The runtime image includes only `ca-certificates` and the binary, running as non-root user 1000. Published to GHCR as a public package (no `imagePullSecrets` required).

---

## Deployment

### Kubernetes Manifests (`k8s/`)

Single-container Deployment with zero secrets. Tailnet exposure via Tailscale Operator Ingress.

**Deployment:** Single `proxy` container from `ghcr.io/basher83/tailnet-microservices/anthropic-oauth-proxy:main`. Config mounted from ConfigMap at `/etc/anthropic-oauth-proxy/config.toml`. `terminationGracePeriodSeconds: 6` (DRAIN_TIMEOUT + 1s buffer). Startup, liveness, and readiness probes on `/health`. Security context: non-root, read-only root filesystem, all capabilities dropped. Resources: 50m/500m CPU, 32Mi/128Mi memory.

**Service:** Plain ClusterIP (no annotations). Tailnet exposure is handled exclusively by the Ingress.

**Ingress:** Tailscale Ingress (`ingressClassName: tailscale`) with `tls.hosts: [anthropic-oauth-proxy]`. Routes tailnet HTTP traffic to the Service ClusterIP:80.

**ConfigMap:** Contains the TOML config per the Configuration section above.

**Kustomization:** namespace, serviceaccount, configmap, deployment, service, ingress.

---

## Success Criteria

- [x] Single binary, <15MB (macOS 4.4MB, Linux x86_64 5.4MB, Linux aarch64 4.7MB)
- [x] Handles 100+ req/s sustained (~2400 req/s measured via `load_test_sustains_100_rps`)
- [x] Zero memory growth over 24h (validated via compressed soak: 20K requests, <5 MiB growth threshold, `memory_soak_test_zero_growth`)
- [x] Works on macOS (arm64) and Linux (amd64/arm64)
- [x] Aperture routes to it successfully (Metric ID 235, verified 2026-02-06)
- [x] Claude Max OAuth tokens work end-to-end (Claude API responses confirmed with header injection, 2026-02-06)
- [x] Graceful shutdown <5s on SIGTERM (DRAIN_TIMEOUT=5s, tested via state machine)
- [x] Single-container pod, zero secrets, no sidecar (operator migration, 2026-02-08)

---

## Implementation Phases

### Phase 1: Scaffold — COMPLETE
- [x] Create project structure matching source file plan
- [x] Define all types from Types section
- [x] Implement `Config` loading with tests
- [x] Stub state machine with all states/events

### Phase 2: HTTP Proxy — COMPLETE
- [x] Implement proxy logic
- [x] Header injection (add + replace)
- [x] Hop-by-hop header stripping
- [x] Add health endpoint
- [x] Add Prometheus metrics endpoint

### Phase 3: Hardening — COMPLETE
- [x] Full state machine with lifecycle transitions
- [x] Graceful shutdown / drain with timeout enforcement
- [x] Prometheus metrics + structured JSON logging
- [x] Cross-compilation (macOS → Linux via cargo-zigbuild)
- [x] Concurrency limiting via ConcurrencyLimitLayer

### Phase 4: Deploy — COMPLETE
- [x] Dockerfile for containerized deployment
- [x] GitHub Actions CI workflow
- [x] Kubernetes manifests with Tailscale Operator annotations
- [x] Operational runbook (RUNBOOK.md)
- [x] Update Aperture config to route to proxy (`anthropic-oauth` provider, `tailnet: true`)
- [x] Monitor production traffic (live E2E traffic verified 2026-02-06)

### Phase 5: Operator Migration — COMPLETE
- [x] Remove `tailscaled` sidecar and `tailscale-localapi` dependency
- [x] Simplify state machine (remove tailnet states, errors, retry logic)
- [x] Simplify config (remove `[tailscale]` section, auth key resolution)
- [x] Update health endpoint (remove tailnet fields, always 200)
- [x] Remove `tailnet_connected` metric
- [x] Update K8s manifests (single container, operator annotations, zero secrets)
- [x] See `specs/operator-migration.md` for full requirements

---

## Resolved Questions

1. **TLS termination** — Inbound TLS is handled by Aperture / tailnet WireGuard encryption. The proxy listens on plain TCP. Outbound to upstream uses `reqwest` with `rustls`.
2. **Multi-tenant** — Single-tenant: one proxy instance injects a fixed set of headers from `[[headers]]` config. Deploy separate instances for different header sets.
3. **Response streaming** — Response bodies are streamed using `reqwest::Response::bytes_stream()` converted to `axum::body::Body::from_stream()`. Metrics (status code, duration) are recorded before the stream begins since headers are available immediately. This avoids buffering entire responses in memory and enables real-time SSE forwarding for Claude API streaming responses.
4. **Tailnet integration** — Originally implemented as a `tailscaled` sidecar with `tailscale-localapi` (Option B from `specs/tailnet.md`). Superseded by the Tailscale Operator via `specs/operator-migration.md`. The Rust binary no longer contains any tailnet code. The Operator handles tailnet authentication, identity, and connectivity externally via Service annotations.

---

## References

- [Anthropic OAuth spec](https://docs.anthropic.com/en/docs/authentication#oauth) — Header requirements
- `specs/operator-migration.md` — Operator migration spec (tailscaled sidecar removal)
- `specs/tailnet.md` — Original tailnet integration strategy (superseded)

---

*Spec-first. Types define contract. State machine is the program.*
