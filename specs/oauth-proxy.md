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
┌─────────────────────────────────────────────────────────────┐
│                        Tailnet                              │
│                                                             │
│  ┌─────────┐      ┌─────────────────────┐      ┌─────────┐ │
│  │ Aperture│ ───► │ anthropic-oauth-proxy│ ───► │External │ │
│  │ (http://ai/)   │ (Rust + tsnet)      │      │Anthropic│ │
│  └─────────┘      └─────────────────────┘      │   API   │ │
│                           │                     └─────────┘ │
│                    MagicDNS: anthropic-oauth-proxy          │
└─────────────────────────────────────────────────────────────┘
```

---

## Types

### Core Types

| Type | Description | Fields |
|------|-------------|--------|
| `Config` | Service configuration | `tailscale: TailscaleConfig`, `proxy: ProxyConfig`, `headers: Vec<HeaderInjection>` |
| `TailscaleConfig` | Tailnet connection settings | `hostname: String`, `auth_key: Option<Secret<String>>`, `state_dir: PathBuf` |
| `ProxyConfig` | HTTP proxy settings | `listen_addr: SocketAddr`, `upstream_url: Url`, `timeout: Duration` |
| `HeaderInjection` | Header to inject | `name: String`, `value: Secret<String>` |
| `ServiceMetrics` | Runtime metrics | `requests_total: u64`, `errors_total: u64`, `latency_p99: Duration` |

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
| `Initializing` | Loading config, setting up resources | `config: Config` |
| `ConnectingTailnet` | Joining the tailnet | `config: Config`, `retries: u32` |
| `Starting` | Starting HTTP listener | `config: Config`, `tailnet: TailnetHandle` |
| `Running` | Accepting and proxying requests | `config: Config`, `tailnet: TailnetHandle`, `listener: HttpListener`, `metrics: ServiceMetrics` |
| `Draining` | Graceful shutdown, finishing in-flight | `pending_requests: u32`, `deadline: Instant` |
| `Stopped` | Terminal state | `exit_code: i32` |
| `Error` | Recoverable error with retry | `error: ServiceError`, `origin: ErrorOrigin`, `retries: u32` |

### `ServiceEvent` Enum

| Event | Description | Payload |
|-------|-------------|---------|
| `ConfigLoaded` | Configuration parsed successfully | `Config` |
| `TailnetConnected` | Joined tailnet, got identity | `TailnetHandle` |
| `TailnetError` | Failed to connect to tailnet | `TailnetError` |
| `ListenerReady` | HTTP listener bound | `HttpListener` |
| `RequestReceived` | Incoming HTTP request | `RequestId`, `Request` |
| `RequestCompleted` | Request finished (success or error) | `RequestId`, `Duration`, `Option<ProxyError>` |
| `ShutdownSignal` | SIGTERM/SIGINT received | — |
| `DrainTimeout` | Drain deadline exceeded | — |
| `RetryTimer` | Retry backoff expired | — |

### `ServiceAction` Enum

| Action | Description | Payload |
|--------|-------------|---------|
| `LoadConfig` | Read and parse config file | `PathBuf` |
| `ConnectTailnet` | Initiate tailnet connection | `TailscaleConfig` |
| `StartListener` | Bind HTTP listener | `SocketAddr` |
| `ProxyRequest` | Forward request to upstream | `RequestId`, `Request`, `Vec<HeaderInjection>` |
| `SendResponse` | Return response to client | `RequestId`, `Response` |
| `ScheduleRetry` | Set retry timer | `Duration` |
| `EmitMetric` | Record metric | `MetricEvent` |
| `Shutdown` | Exit process | `i32` |

---

## State Transitions

| Current State | Event | New State | Action |
|---------------|-------|-----------|--------|
| `Initializing` | `ConfigLoaded(cfg)` | `ConnectingTailnet` | `ConnectTailnet` |
| `ConnectingTailnet` | `TailnetConnected(h)` | `Starting` | `StartListener` |
| `ConnectingTailnet` | `TailnetError` (retries < 5) | `Error` | `ScheduleRetry(backoff)` |
| `ConnectingTailnet` | `TailnetError` (retries >= 5) | `Stopped` | `Shutdown(1)` |
| `Error` (origin=Tailnet) | `RetryTimer` | `ConnectingTailnet` | `ConnectTailnet` |
| `Starting` | `ListenerReady(l)` | `Running` | — |
| `Running` | `RequestReceived(id, req)` | `Running` | `ProxyRequest(id, req)` |
| `Running` | `RequestCompleted(id, dur, err)` | `Running` | `EmitMetric(...)` |
| `Running` | `ShutdownSignal` | `Draining` | — |
| `Draining` | `RequestCompleted` (pending=0) | `Stopped` | `Shutdown(0)` |
| `Draining` | `DrainTimeout` | `Stopped` | `Shutdown(0)` |
| *any* | `ShutdownSignal` (if urgent) | `Stopped` | `Shutdown(0)` |

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
| `authorization` header | Pass through unchanged |
| Hop-by-hop headers | Strip before forwarding |

### Hop-by-hop Headers (strip)

```
connection, keep-alive, proxy-authenticate, proxy-authorization,
te, trailer, transfer-encoding, upgrade
```

---

## Error Handling

### `ServiceError` Enum

| Variant | Description | Retryable |
|---------|-------------|-----------|
| `ConfigError` | Failed to load/parse config | No |
| `TailnetAuthError` | Invalid auth key | No |
| `TailnetConnectError` | Network/coordination failure | Yes (backoff) |
| `ListenerBindError` | Port in use | No |
| `UpstreamTimeout` | Request to Anthropic timed out | Yes (per-request) |
| `UpstreamError` | Non-2xx from upstream | No (pass through) |
| `InvalidRequest` | Malformed client request | No |

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
listen_addr = "0.0.0.0:443"
upstream_url = "https://api.anthropic.com"
timeout_secs = 60
max_connections = 1000

[[headers]]
name = "anthropic-beta"
value = "oauth-2025-04-20"
```

### Environment Variables

| Variable | Description | Overrides |
|----------|-------------|-----------|
| `TS_AUTHKEY` | Tailscale auth key | `tailscale.auth_key` |
| `CONFIG_PATH` | Config file path | CLI `--config` |
| `LOG_LEVEL` | Logging verbosity | `RUST_LOG` |

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

200 OK
{
  "status": "healthy",
  "tailnet": "connected",
  "uptime_seconds": 3600,
  "requests_served": 12345
}
```

---

## Build & Distribution

### Cargo.toml

```toml
[package]
name = "anthropic-oauth-proxy"
version = "0.1.0"
edition = "2024"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# HTTP
axum = "0.8"
reqwest = { version = "0.12", features = ["rustls-tls"] }
hyper = "1"

# Tailscale (evaluate options)
# tsnet-rs = "0.1"  # If mature enough
# OR embedded WireGuard:
# defguard_boringtun = "0.6"
# smoltcp = "0.11"

# Config
serde = { version = "1", features = ["derive"] }
toml = "0.8"

# Observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json"] }

# Security
zeroize = { version = "1", features = ["derive"] }

# Error handling
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

### Phase 5: Deploy
- [ ] Update Aperture config to route to proxy
- [ ] Monitor production traffic
- [ ] Document runbook

---

## Resolved Questions

1. **tsnet-rs maturity** — Chose Option B (`tailscaled` sidecar + `tailscale-localapi` v0.4.2) over libtailscale FFI. The sidecar pattern avoids Go build dependencies and uses a production-grade crate. See `IMPLEMENTATION_PLAN.md` for details.
2. **TLS termination** — Inbound TLS is handled by Aperture / tailnet WireGuard encryption. The proxy listens on plain TCP. Outbound to upstream uses `reqwest` with `rustls-tls`.
3. **Multi-tenant** — Single-tenant: one proxy instance injects a fixed set of headers from `[[headers]]` config. Deploy separate instances for different header sets.
4. **State persistence** — Since Option B was chosen, `state_dir` is deserialized from TOML for schema compliance but the Rust service does not use it. `tailscaled` manages its own state externally.

---

## References

- [ghuntley/loom/specs/wgtunnel-system.md](https://github.com/ghuntley/loom) — WireGuard stack reference
- [Tailscale tsnet docs](https://tailscale.com/kb/1244/tsnet) — Embedded tailnet library
- [Anthropic OAuth spec](https://docs.anthropic.com/en/docs/authentication#oauth) — Header requirements
- [Teardown: ghuntley's Vibe Coding](../Areas/recon/Teardown-Ghuntley-Vibe-Coding.md) — Methodology reference

---

*Spec-first. Types define contract. State machine is the program.* 🦀
