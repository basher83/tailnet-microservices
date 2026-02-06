# Spec: Tailnet Microservices in Rust

**Status:** Draft (ghuntley pattern v2)  
**Created:** 2026-02-05  
**Updated:** 2026-02-05  
**Author:** Astrogator + Brent  

---

## Overview

Single-binary Rust service with a `tailscaled` sidecar for tailnet connectivity. The sidecar approach (Option B from `specs/tailnet.md`) was chosen over libtailscale FFI for production maturity and zero Go build dependencies.

**Design Principles:**

| Principle | Implementation |
|-----------|----------------|
| Single responsibility | One binary, one function |
| Minimal external deps | Static binary, requires only `tailscaled` sidecar |
| Tailnet-native | MagicDNS identity, ACL-controlled |
| Pure state machines | Event → Action, caller handles I/O |
| Type-safe | Rust compiler catches spec deviations |

**Source files:**

- `services/oauth-proxy/src/main.rs` — Entry point, CLI parsing, axum server
- `services/oauth-proxy/src/config.rs` — Configuration types and loading
- `services/oauth-proxy/src/service.rs` — Service state machine
- `services/oauth-proxy/src/tailnet.rs` — Tailscale integration (tailscale-localapi)
- `services/oauth-proxy/src/proxy.rs` — HTTP proxy logic
- `services/oauth-proxy/src/metrics.rs` — Prometheus metrics exposition
- `services/oauth-proxy/src/error.rs` — Error types

---

## First Target: Anthropic OAuth Proxy

### Problem
Aperture lacks custom header injection. Claude Max OAuth tokens require the `anthropic-beta: oauth-2025-04-20` header.

### Solution
Rust binary that joins tailnet, injects required headers, proxies to Anthropic API.

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                           Tailnet                                │
│                                                                  │
│  ┌─────────┐      ┌──────────────────────────┐      ┌─────────┐ │
│  │ Aperture│ ───► │  anthropic-oauth-proxy    │ ───► │External │ │
│  │ (http://ai/)   │  (Rust + tailscaled       │      │Anthropic│ │
│  └─────────┘      │   sidecar)                │      │   API   │ │
│                    └──────────────────────────┘      └─────────┘ │
│                           │                                      │
│                    MagicDNS: anthropic-oauth-proxy                │
└──────────────────────────────────────────────────────────────────┘
```

---

## Types

### Core Types

| Type | Description | Fields |
|------|-------------|--------|
| `Config` | Service configuration | `tailscale: TailscaleConfig`, `proxy: ProxyConfig`, `headers: Vec<HeaderInjection>` |
| `TailscaleConfig` | Tailnet connection settings | `hostname: String`, `auth_key: Option<Secret<String>>`, `auth_key_file: Option<PathBuf>`, `state_dir: PathBuf` |
| `ProxyConfig` | HTTP proxy settings | `listen_addr: SocketAddr`, `upstream_url: String`, `timeout_secs: u64`, `max_connections: usize` |
| `HeaderInjection` | Header to inject | `name: String`, `value: String` (not sensitive; e.g. `anthropic-beta` value) |
| `ServiceMetrics` | Runtime metrics | `requests_total: Arc<AtomicU64>`, `errors_total: Arc<AtomicU64>`, `in_flight: Arc<AtomicU64>`, `started_at: Instant` |

### Secret Wrapper

```rust
use zeroize::Zeroize;

/// Sensitive value - redacted in Debug/Display/logs
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    pub fn expose(&self) -> &T { &self.0 }
}

impl<T: Zeroize> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[REDACTED]")
    }
}
```

---

## State Machine

The service uses an explicit state machine for lifecycle management.

### `ServiceState` Enum

| State | Description | Fields |
|-------|-------------|--------|
| `Initializing` | Loading config, setting up resources | (no data) |
| `ConnectingTailnet` | Joining the tailnet | `retries: u32`, `listen_addr: SocketAddr` |
| `Starting` | Starting HTTP listener | `tailnet: TailnetHandle`, `listen_addr: SocketAddr` |
| `Running` | Accepting and proxying requests | `tailnet: TailnetHandle`, `listen_addr: SocketAddr`, `metrics: ServiceMetrics` |
| `Draining` | Graceful shutdown, finishing in-flight | `deadline: Instant` (drain coordination handled by axum's graceful shutdown and the `in_flight` atomic counter) |
| `Stopped` | Terminal state | `exit_code: i32` |
| `Error` | Recoverable error with retry | `error: String`, `origin: ErrorOrigin`, `retries: u32`, `listen_addr: SocketAddr` |

### `ServiceEvent` Enum

| Event | Description | Payload |
|-------|-------------|---------|
| `ConfigLoaded` | Configuration parsed successfully | `listen_addr: SocketAddr` |
| `TailnetConnected` | Joined tailnet, got identity | `TailnetHandle` |
| `TailnetError` | Failed to connect to tailnet | `String` (error message; type discrimination happens in the caller before feeding events) |
| `ListenerReady` | HTTP listener bound | (no data) |
| `RequestReceived` | Incoming HTTP request | `request_id: String` (request object handled directly by proxy handler) |
| `RequestCompleted` | Request finished (success or error) | `request_id: String`, `duration: Duration`, `error: Option<ServiceError>` |
| `ShutdownSignal` | SIGTERM/SIGINT received | — |
| `DrainTimeout` | Drain deadline exceeded | — |
| `RetryTimer` | Retry backoff expired | — |

### `ServiceAction` Enum

| Action | Description | Payload |
|--------|-------------|---------|
| `ConnectTailnet` | Initiate tailnet connection | (no data) |
| `StartListener` | Bind HTTP listener | `addr: SocketAddr` |
| `ScheduleRetry` | Set retry timer | `delay: Duration` |
| `Shutdown` | Exit process | `exit_code: i32` |
| `None` | No-op | — |

Config loading, request proxying, response sending, and metric emission happen outside the state machine. `LoadConfig` occurs before the state machine starts. `ProxyRequest`/`SendResponse` are handled directly by the axum proxy handler. `EmitMetric` calls are inlined at the call site.

The state machine drives the startup lifecycle (`Initializing` through `Running`). Once `Running`, graceful shutdown is handled by axum's `with_graceful_shutdown` mechanism with a `DRAIN_TIMEOUT` enforcement, rather than by firing `ShutdownSignal`/`DrainTimeout` events through the state machine. The `Draining` and `Stopped` transitions are implemented and tested for correctness but are not exercised at runtime.

`RequestReceived` and `RequestCompleted` events exist in the enum for completeness but are handled directly by the proxy handler via atomic counters, not through the state machine. Calling `handle_event` with these events from `Running` state will `unreachable!()` — this is by design.

---

## State Transitions

| Current State | Event | New State | Action |
|---------------|-------|-----------|--------|
| `Initializing` | `ConfigLoaded(cfg)` | `ConnectingTailnet` | `ConnectTailnet` |
| `ConnectingTailnet` | `TailnetConnected(h)` | `Starting` | `StartListener` |
| `ConnectingTailnet` | `TailnetError` (retries < 5) | `Error` | `ScheduleRetry(backoff)` |
| `ConnectingTailnet` | `TailnetError` (retries >= 5) | `Stopped` | `Shutdown(1)` |
| `Error` (origin=Tailnet) | `RetryTimer` | `ConnectingTailnet` | `ConnectTailnet` |
| `Starting` | `ListenerReady` | `Running` | `None` |
| `Running` | `RequestReceived`/`RequestCompleted` | `Running` | (handled by proxy handler, not state machine) |
| `Running` | `ShutdownSignal` | `Draining` | `None` |
| `Draining` | `DrainTimeout` | `Stopped` | `Shutdown(0)` |
| `Draining` | `ShutdownSignal` | `Stopped` | `Shutdown(0)` |
| `Stopped` | *any* | `Stopped` | `None` (terminal, inert) |
| *any non-Running* | `ShutdownSignal` | `Stopped` | `Shutdown(0)` |

---

## HTTP Proxy Protocol

### Request Flow

```
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

```
connection, keep-alive, proxy-authenticate, proxy-authorization,
te, trailer, transfer-encoding, upgrade
```

### Body Size Limit

Requests with bodies exceeding 10 MiB (10,485,760 bytes) are rejected with 400 Bad Request.

### Response Streaming

Response bodies are streamed from upstream to the client without buffering. This is critical for SSE (Server-Sent Events) where the Anthropic API streams Claude's responses in real-time. The proxy collects the upstream status code and headers before streaming begins, enabling metrics recording for all responses including long-lived SSE connections. Mid-stream errors result in connection closure (HTTP status already sent); SSE clients handle reconnection automatically.

---

## Error Handling

### Error Enum (service-level)

The service-level `Error` enum in `error.rs` contains errors that flow through the state machine:

| Variant | Description | Retryable |
|---------|-------------|-----------|
| `TailnetAuth` | Invalid or expired auth key | No |
| `TailnetMachineAuth` | Node needs admin approval in Tailscale console | No |
| `TailnetConnect` | Network/coordination failure | Yes (backoff) |
| `TailnetNotRunning` | Daemon not available or not configured | No |

### Errors handled outside the state machine

These errors are handled directly at the call site rather than through the `Error` enum:

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
| `TailnetConnectError` | 5 | Exponential: 1s, 2s, 4s, 8s, 16s |
| `UpstreamTimeout` | 2 | Fixed: 100ms |

### Error Response Format

```json
{
  "error": {
    "type": "proxy_error",
    "message": "Upstream timeout after 30s",
    "request_id": "req_abc123"
  }
}
```

---

## Configuration

### File Format (TOML)

```toml
# anthropic-oauth-proxy.toml

[tailscale]
hostname = "anthropic-oauth-proxy"
# Auth key from environment: TS_AUTHKEY
# Or specify path to file:
# auth_key_file = "/run/secrets/ts-authkey"
state_dir = "/var/lib/anthropic-oauth-proxy/tailscale"

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
| `TS_AUTHKEY` | Tailscale auth key | Sets `auth_key` directly; `auth_key_file` is not read when set |
| `CONFIG_PATH` | Config file path | Fallback when CLI `--config` is not provided |
| `LOG_LEVEL` | Logging verbosity | Checked first; falls back to `RUST_LOG` |

### Precedence

```
CLI args > Environment vars > Config file > Defaults
```

---

## Observability

### Structured Logging

```rust
use tracing::{info, warn, error, instrument};

#[instrument(skip(req), fields(request_id = %id))]
async fn handle_request(id: RequestId, req: Request) {
    info!("received request");
    // ...
    info!(latency_ms = ?elapsed.as_millis(), "request completed");
}
```

### Metrics (stdout JSON or Prometheus)

| Metric | Type | Labels |
|--------|------|--------|
| `proxy_requests_total` | Counter | `status`, `method` |
| `proxy_request_duration_seconds` | Histogram | `status` |
| `proxy_upstream_errors_total` | Counter | `error_type` |
| `tailnet_connected` | Gauge | — |

### Health Endpoint

```
GET /health

200 OK (tailnet connected)
{
  "status": "healthy",
  "tailnet": "connected",
  "tailnet_hostname": "anthropic-oauth-proxy",
  "tailnet_ip": "100.64.0.1",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}

503 Service Unavailable (tailnet not connected)
{
  "status": "degraded",
  "tailnet": "not_connected",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}
```

---

## Build & Distribution

### Cargo.toml

Package name is `oauth-proxy` with binary name `anthropic-oauth-proxy` (via `[[bin]]` table). Dependencies are managed via workspace `Cargo.toml`:

```toml
# Key workspace dependencies
tokio = { version = "1", features = ["full"] }
axum = "0.8"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "http2", "stream"] }
hyper = "1"
tailscale-localapi = "0.4"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
metrics = "0.24"
metrics-exporter-prometheus = "0.16"
tower = { version = "0.5", features = ["limit"] }
zeroize = { version = "1", features = ["derive"] }
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

---

## Success Criteria

- [x] Single binary, <15MB (macOS 4.4MB, Linux x86_64 5.4MB, Linux aarch64 4.7MB)
- [ ] Joins tailnet in <5s on startup (requires live tailnet)
- [ ] Handles 100+ req/s sustained (requires load testing)
- [ ] Zero memory growth over 24h (requires soak testing)
- [x] Works on macOS (arm64) and Linux (amd64/arm64)
- [ ] Aperture routes to it successfully (requires live tailnet + Aperture)
- [ ] Claude Max OAuth tokens work end-to-end (requires live infrastructure)
- [x] Graceful shutdown <5s on SIGTERM (DRAIN_TIMEOUT=5s, tested via state machine)

---

## Implementation Phases

### Phase 1: Scaffold — COMPLETE
- [x] Create project structure matching source file plan
- [x] Define all types from Types section
- [x] Implement `Config` loading with tests
- [x] Stub state machine with all states/events

### Phase 2: HTTP Proxy — COMPLETE
- [x] Implement proxy logic without tailnet
- [x] Header injection (add + replace)
- [x] Hop-by-hop header stripping
- [x] Add health endpoint
- [x] Add Prometheus metrics endpoint

### Phase 3: Tailnet Integration — COMPLETE
- [x] Chose Option B (tailscaled sidecar + `tailscale-localapi`)
- [x] Implement `ConnectingTailnet` → `Running` flow
- [ ] Test MagicDNS resolution (requires live tailnet)
- [ ] ACL verification (requires live tailnet + Aperture)

### Phase 4: Hardening — COMPLETE
- [x] Full state machine with error recovery
- [x] Graceful shutdown / drain
- [x] Prometheus metrics + structured JSON logging
- [x] Cross-compilation (macOS → Linux via cargo-zigbuild)
- [x] Concurrency limiting via ConcurrencyLimitLayer

### Phase 5: Deploy — PARTIALLY COMPLETE
- [x] Dockerfile for containerized deployment
- [x] GitHub Actions CI workflow
- [x] Kubernetes manifests with tailscaled sidecar
- [x] Operational runbook (RUNBOOK.md)
- [ ] Update Aperture config to route to proxy (requires live tailnet)
- [ ] Monitor production traffic (requires live infrastructure)

---

## Resolved Questions

1. **tsnet-rs maturity** — Chose Option B (`tailscaled` sidecar + `tailscale-localapi` v0.4.2) over libtailscale FFI. The sidecar pattern avoids Go build dependencies and uses a production-grade crate. See `IMPLEMENTATION_PLAN.md` for details.
2. **TLS termination** — Inbound TLS is handled by Aperture / tailnet WireGuard encryption. The proxy listens on plain TCP. Outbound to upstream uses `reqwest` with `rustls-tls`.
3. **Multi-tenant** — Single-tenant: one proxy instance injects a fixed set of headers from `[[headers]]` config. Deploy separate instances for different header sets.
4. **State persistence** — Since Option B was chosen, `state_dir` is deserialized from TOML for schema compliance but the Rust service does not use it. `tailscaled` manages its own state externally.
5. **Auth key usage** — Since Option B was chosen, `auth_key` and `auth_key_file` are loaded from config/env for schema compliance but are not passed to the tailnet module. The Rust service queries an already-authenticated `tailscaled`; authentication is the sidecar's responsibility.
6. **Tailnet disconnect** — Spec lifecycle step 5 says "disconnect cleanly." With the sidecar model, the Rust service does not own the tailnet connection, so disconnect is a no-op. On shutdown, the `tailnet_connected` Prometheus gauge is set to 0 for observability. The `tailscaled` sidecar handles its own lifecycle via the pod termination signal.
7. **Response streaming** — Response bodies are streamed using `reqwest::Response::bytes_stream()` converted to `axum::body::Body::from_stream()`. Metrics (status code, duration) are recorded before the stream begins since headers are available immediately. This avoids buffering entire responses in memory and enables real-time SSE forwarding for Claude API streaming responses.

---

## References

- [ghuntley/loom/specs/wgtunnel-system.md](https://github.com/ghuntley/loom) — WireGuard stack reference
- [Tailscale tsnet docs](https://tailscale.com/kb/1244/tsnet) — Embedded tailnet library
- [Anthropic OAuth spec](https://docs.anthropic.com/en/docs/authentication#oauth) — Header requirements
- [Teardown: ghuntley's Vibe Coding](../Areas/recon/Teardown-Ghuntley-Vibe-Coding.md) — Methodology reference

---

*Spec-first. Types define contract. State machine is the program.* 🦀
