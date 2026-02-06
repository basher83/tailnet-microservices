# Implementation Plan

Phases 1-5 complete. All 69 tests pass. Binary sizes well under 15MB target. Specs updated with resolved decisions.

Fifth spec audit (v0.0.22): Found 1 bug, 3 spec gaps, and 3 missing test assertions. All addressed: 1 bug fixed, 3 spec gaps resolved, 1 new test added (69 total).

## Remaining Work (requires live infrastructure)

- [ ] Aperture config update — route `http://ai/` to the proxy (requires live tailnet)
- [ ] Production monitoring — observe live traffic
- [ ] Test MagicDNS hostname resolution (requires live tailnet)
- [ ] Verify ACL connectivity from Aperture (requires live tailnet + Aperture)

## Bug Fixed & Improvements (v0.0.22)

Fifth spec audit found 1 bug, 3 spec gaps, and 3 missing test assertions:

- BUG: Multi-value request headers were collapsed by `insert()` — if a client sent multiple values for the same header name (e.g. multiple Cookie values), only the last survived. Fixed by using `append()` which preserves all values. The response path already used append-style correctly.
- Added per-request completion log with latency (`info!(status, latency_ms, "request completed")`) per the spec's observability example. Previously only errors/retries were logged; successful requests were silent.
- Spec: Environment variable precedence table column renamed from "Fallback for" to "Precedence" with per-entry descriptions that accurately match both the stated precedence rule and the implementation.
- Spec: Added note clarifying that the state machine drives startup lifecycle only; graceful shutdown is handled by axum's `with_graceful_shutdown` mechanism rather than by firing events through the state machine.
- Test: Prometheus metrics test now verifies all 4 spec-defined metrics (`proxy_requests_total`, `proxy_request_duration_seconds`, `proxy_upstream_errors_total`, `tailnet_connected`). Previously only checked 2 of 4.
- Test: Added multi-value request header preservation test to verify the `insert()` -> `append()` fix.

## Known Limitations

- Health endpoint `tailnet` state is set once at startup and never updated during operation. If tailscaled drops during runtime, health still reports `"connected"`. Fixing this requires tailnet health monitoring (periodic polling of tailscaled), which is infrastructure work beyond the current spec. The `tailnet_connected` Prometheus gauge does get set to `false` during graceful shutdown.
- `ConfigError` and `ListenerBindError` from the spec's `ServiceError` enum are not in the service's Rust error enum. Config errors use `common::Error` and listener bind errors use `anyhow`. These paths work correctly; the gap is only in enum structure.

## Learnings

- Reverse proxies must strip the client's `host` header before forwarding. The client sends `Host: <proxy-address>` but the upstream expects `Host: <upstream-address>`. HTTP client libraries like reqwest automatically set the correct Host from the URL, but only if the incoming Host isn't manually set in the header map.
- Config-driven header injection must protect safety-critical headers. The `authorization` header should never be overwritable via config, even if someone misconfigures it. This is a system boundary validation.
- When copying HTTP headers in a proxy, use `append()` not `insert()` to preserve multi-value headers. `insert()` replaces, `append()` accumulates. This matters for headers like Cookie, Accept-Encoding, and custom multi-value headers.
- Rust 2024 edition requires `unsafe {}` blocks inside `unsafe fn` bodies. Tests that call `std::env::set_var`/`remove_var` (unsafe since Rust 1.83) need both the `unsafe fn` wrapper and inner `unsafe {}` blocks.
- Tests that mutate environment variables must be serialized with a `Mutex` to prevent data races when `cargo test` runs in parallel (default behavior). Without this, env-var-dependent tests fail nondeterministically.
- `tracing-subscriber` requires the `env-filter` feature for `EnvFilter` support. The `json` feature alone is not sufficient.
- Drain coordination: axum's `with_graceful_shutdown` handles connection-level draining (stops accepting new connections, waits for in-flight to finish), but it waits indefinitely by default. The spec requires a 5-second drain timeout. Enforced by spawning the server as a task, signaling it via a `oneshot` channel on SIGTERM/SIGINT, then racing the drain against `DRAIN_TIMEOUT` using `tokio::time::timeout`.
- `tailscale-localapi` v0.4.2 uses `chrono` for timestamps and brings in `hyper` v0.14 (in addition to the workspace's `hyper` v1). This is expected — the crate was built against an older `hyper` API.
- On macOS with the App Store Tailscale variant, there is no Unix socket. The fallback is `tailscale status --json` which parses the same `Status` type via `serde_json`.
- `metrics-exporter-prometheus` global recorder can only be installed once per process. In tests, use `PrometheusBuilder::build_recorder()` + `.handle()` to create isolated instances without global installation.
- Integration tests using `tower::ServiceExt::oneshot` give full end-to-end coverage without needing to bind a TCP port — they call the axum router directly as a tower Service.
- Cross-compilation from macOS to Linux requires `cargo-zigbuild` (uses zig as the C cross-linker). Standard `cargo build --target` fails because `aws-lc-sys` needs a C cross-compiler.
- `reqwest` with default features enabled pulls in `native-tls` → `openssl-sys` on Linux targets, even when `rustls-tls` is also enabled. Setting `default-features = false` is required to avoid the OpenSSL dependency.
- `tower` crate requires explicit feature flags for each layer type. `ConcurrencyLimitLayer` requires the `limit` feature.
- `BackendState` enum in `tailscale-localapi` is `#[non_exhaustive]`, requiring wildcard match arms.
- Tower's `ConcurrencyLimitLayer` queues excess requests rather than rejecting them. Requests above `max_connections` will wait (not fail) until a slot opens.
- Docker build uses native `x86_64-unknown-linux-gnu` target (not musl) inside `rust:1-bookworm`. No cross-compilation needed since Docker IS Linux.
- K8s manifests use `TS_USERSPACE=true` for the tailscaled sidecar to avoid requiring `NET_ADMIN` capabilities. The proxy and tailscaled share the Unix socket via an `emptyDir` volume.
- GitHub Actions CI uses `dtolnay/rust-toolchain@stable` and `Swatinem/rust-cache@v2`. Docker job uses `docker/build-push-action@v6` with GHA cache. Images push to GHCR using the built-in `GITHUB_TOKEN`.
- `BackendState::NeedsMachineAuth` requires manual admin approval in the Tailscale console. Mapping it to a retryable error wastes 31 seconds of exponential backoff before giving up. It must be non-retryable.
- A spec-vs-implementation audit is valuable after completing major phases. Found 39+ discrepancies across five audits including 4 bugs, spec documentation gaps, and positive deviations.

## Environment Notes

- Rust toolchain: cargo 1.93.0, rustc 1.93.0 (stable, Jan 2026), installed at `~/.cargo/bin/cargo`
- Cross-compilation: `cargo-zigbuild` v0.21.6 + zig 0.15.2 for Linux targets
