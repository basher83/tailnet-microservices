# Implementation Plan

Phases 1-5 complete. All 83 tests pass. Binary sizes well under 15MB target. Specs updated with resolved decisions.

Nineteenth audit (v0.0.33): Comprehensive production-readiness audit found 1 deployment blocker, 2 unused dependencies, 4 hardening gaps, 1 dead code allocation, and 1 test coverage gap. All resolved: Dockerfile now creates non-root user (UID 1000) for K8s `runAsNonRoot`; removed unused `hyper` (workspace + oauth-proxy) and `serde` (common crate) dependencies; hardened K8s securityContext with `allowPrivilegeEscalation: false`, `readOnlyRootFilesystem: true`, `capabilities.drop: [ALL]`; pinned tailscaled sidecar to v1.94.1 (was `:latest`); configured `reqwest::Client` with `connect_timeout(5s)` and `pool_max_idle_per_host(100)`; removed dead `ServiceMetrics` from state machine `Running` variant (metrics are owned by `ProxyState`); added CLI arg precedence test; spec updated to remove `hyper` from dependency list. 83 tests passing.

## Remaining Work (requires live infrastructure)

- [ ] Aperture config update — route `http://ai/` to the proxy (requires live tailnet)
- [ ] Production monitoring — observe live traffic
- [ ] Test MagicDNS hostname resolution (requires live tailnet)
- [ ] Verify ACL connectivity from Aperture (requires live tailnet + Aperture)

## Known Limitations

- Health endpoint `tailnet` state is set once at startup and never updated during operation. If tailscaled drops during runtime, health still reports `"connected"`. Fixing this requires tailnet health monitoring (periodic polling of tailscaled), which is infrastructure work beyond the current spec. The `tailnet_connected` Prometheus gauge does get set to `false` during graceful shutdown.
- `ConfigError` and `ListenerBindError` are not in the service's Rust error enum. Config errors use `common::Error` and listener bind errors use `anyhow`. These paths work correctly; the spec now documents this split explicitly.

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
- A spec-vs-implementation audit is valuable after completing major phases. Found 43+ discrepancies across ten audits including 5 bugs, spec documentation gaps, and positive deviations. The tenth audit found 1 state machine bug (terminal state not fully inert).
- Terminal states in a state machine must be explicitly guarded before wildcard match arms. Without a `Stopped` guard before `(_, ShutdownSignal)`, the wildcard produces a `Shutdown` action from an already-stopped state, violating the "terminal means inert" invariant.
- K8s sidecar pattern requires both containers to mount the shared volume. The volume definition in `spec.volumes` is not enough — each container that needs the socket must have a `volumeMount` entry. Easy to miss because the tailscaled container (which creates the socket) works fine; only the consumer (proxy) fails.
- Response bodies must be streamed, not buffered, in a proxy targeting the Claude API. The Anthropic API uses SSE (Server-Sent Events) for streaming responses. Buffering breaks real-time delivery and uses unbounded memory. Use `reqwest::Response::bytes_stream()` with `axum::body::Body::from_stream()`. Metrics (status, duration) must be collected before consuming the stream since headers are available immediately.
- Config validation at system boundaries catches misconfigurations early: `upstream_url` must have an http(s) scheme, `timeout_secs` and `max_connections` must be non-zero. Without URL scheme validation, reqwest fails at request time with a confusing error instead of at startup.
- `metrics-exporter-prometheus` renders `metrics::histogram!()` as a Prometheus summary (quantiles) by default. To get a true histogram (with `_bucket` lines needed by `histogram_quantile()` queries), you must configure explicit bucket boundaries via `set_buckets_for_metric()`. Without this, RUNBOOK PromQL queries referencing `_bucket` will fail silently.
- In a sidecar pattern, secrets should only be mounted in the container that consumes them. `TS_AUTHKEY` belongs on the tailscaled sidecar, not the proxy container — the proxy queries tailnet state via the Unix socket and never authenticates directly.
- Spec dependency lists can drift from the actual Cargo.toml when features are added during implementation. The `"stream"` feature on reqwest was added for response streaming but the spec's Build & Distribution section was not updated. Always update the spec when adding dependency features.
- Dockerfiles for K8s pods with `runAsNonRoot: true` must create the non-root user in the image. `debian:bookworm-slim` only has root; use `useradd -u 1000 -r -s /sbin/nologin proxy` and `USER 1000` in the runtime stage. Without this, the pod crashes with `CreateContainerConfigError`.
- `reqwest::Client::new()` uses unbounded connection pool defaults. For a proxy with configurable `max_connections`, set `connect_timeout()` and `pool_max_idle_per_host()` on the builder to prevent unbounded TCP connections when upstream is slow to accept.
- K8s Pod Security Standards (restricted profile) require `allowPrivilegeEscalation: false`, `readOnlyRootFilesystem: true`, and `capabilities: { drop: ["ALL"] }` on every container. Missing these can block deployment to hardened clusters.
- Using `:latest` for sidecar images in K8s deployments breaks reproducibility and rollbacks. Pin to specific versions (e.g. `tailscale:v1.94.1`) so that `kubectl rollout undo` works predictably.
- State machine variants should only carry data they own and use. The `Running` state had a `ServiceMetrics` that was never read because `main.rs` creates its own metrics instance wired to `ProxyState`. Dead allocations in state variants waste memory and confuse readers.

## Environment Notes

- Rust toolchain: cargo 1.93.0, rustc 1.93.0 (stable, Jan 2026), installed at `~/.cargo/bin/cargo`
- Cross-compilation: `cargo-zigbuild` v0.21.6 + zig 0.15.2 for Linux targets
