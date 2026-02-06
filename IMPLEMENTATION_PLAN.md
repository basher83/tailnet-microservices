# Implementation Plan

Phases 1-5 complete. All 68 tests pass. Binary sizes well under 15MB target. Specs updated with resolved decisions.

Fourth spec audit (v0.0.21): Found 0 bugs, 1 spec gap, and 12 missing test assertions. All addressed: 12 new tests added (68 total), spec precedence table fixed.

## Remaining Work (requires live infrastructure)

- [ ] Aperture config update — route `http://ai/` to the proxy (requires live tailnet)
- [ ] Production monitoring — observe live traffic
- [ ] Test MagicDNS hostname resolution (requires live tailnet)
- [ ] Verify ACL connectivity from Aperture (requires live tailnet + Aperture)

## Test Coverage Added (v0.0.21)

Fourth spec audit found 0 bugs but 12 missing test assertions and 1 spec gap:

- Secret Display trait redaction (common crate)
- ShutdownSignal from all states: Initializing, Starting, Error, Draining (service.rs)
- Stopped state is terminal: events don't escape terminal state (service.rs)
- Stopped + ShutdownSignal stays stopped (service.rs)
- Health endpoint Content-Type is application/json (main.rs)
- Error response Content-Type is application/json (proxy.rs)
- Authorization injection blocked even when client sends no auth header (main.rs)
- auth_key_file with empty/whitespace content yields no auth key (config.rs)
- auth_key_file pointing to nonexistent path returns error (config.rs)
- Spec gap: env var table "Overrides" column renamed to "Fallback for" to match stated precedence (specs/oauth-proxy.md)

## Bugs Fixed (v0.0.20)

- `host` header from client was forwarded verbatim to upstream, sending the proxy's tailnet hostname (e.g. `anthropic-oauth-proxy`) instead of letting reqwest derive the correct host from the upstream URL. This would cause 400/421 errors from upstream servers that validate the Host header.
- `authorization` header was not protected from injection overwrite. If someone misconfigured `[[headers]]` with `name = "authorization"`, the client's Bearer token would be silently replaced. The spec explicitly states authorization must pass through unchanged.
- Oversized body error test was missing `request_id` format verification (now asserts `req_` prefix).

## Known Limitations

- Health endpoint `tailnet` state is set once at startup and never updated during operation. If tailscaled drops during runtime, health still reports `"connected"`. Fixing this requires tailnet health monitoring (periodic polling of tailscaled), which is infrastructure work beyond the current spec. The `tailnet_connected` Prometheus gauge does get set to `false` during graceful shutdown.
- `ConfigError` and `ListenerBindError` from the spec's `ServiceError` enum are not in the service's Rust error enum. Config errors use `common::Error` and listener bind errors use `anyhow`. These paths work correctly; the gap is only in enum structure.

## Bugs Fixed (v0.0.18)

- `NeedsMachineAuth` backend state was mapped to `TailnetConnect` (retryable), wasting 31 seconds retrying when manual admin approval is needed. Now maps to non-retryable `TailnetMachineAuth` which bails immediately with a clear message.
- Health endpoint returned 200/healthy even when tailnet was not connected. A proxy without tailnet is degraded, not healthy. Now returns 503 with `"status": "degraded"` when tailnet is not connected.
- Concurrency limit test was named `concurrency_limit_rejects_excess_requests` but Tower's `ConcurrencyLimitLayer` queues (not rejects). Renamed to `concurrency_limit_queues_excess_requests`.

## Learnings

- Reverse proxies must strip the client's `host` header before forwarding. The client sends `Host: <proxy-address>` but the upstream expects `Host: <upstream-address>`. HTTP client libraries like reqwest automatically set the correct Host from the URL, but only if the incoming Host isn't manually set in the header map.
- Config-driven header injection must protect safety-critical headers. The `authorization` header should never be overwritable via config, even if someone misconfigures it. This is a system boundary validation.

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
- A spec-vs-implementation audit is valuable after completing major phases. Found 39 discrepancies in this project including 3 bugs, spec documentation gaps, and positive deviations.

## Environment Notes

- Rust toolchain: cargo 1.93.0, rustc 1.93.0 (stable, Jan 2026), installed at `~/.cargo/bin/cargo`
- Cross-compilation: `cargo-zigbuild` v0.21.6 + zig 0.15.2 for Linux targets
