# Implementation Plan

Phases 1-5 complete. All 83 tests pass. Binary sizes well under 15MB target. Specs updated with resolved decisions.

Audits 1-21: Found and fixed 43+ issues across ten initial audits including 5 bugs, spec documentation gaps, and positive deviations. Subsequent audits (11-21) found incremental issues in state machine transitions, K8s manifests, Docker configuration, metrics, and documentation.

Twenty-second audit (v0.0.36): Fixed 3 issues — kustomization.yaml overwrote real secrets, spec `Running` state had stale `metrics` field, AGENTS.md recommended deprecated `async-trait`.

Twenty-third audit (v0.0.37): Fixed 2 issues — RUNBOOK described histogram as summary, `TAILSCALE_SOCKET` env var undocumented.

Twenty-fourth audit (v0.0.37): Comprehensive audit, 0 issues found.

Twenty-fifth audit (v0.0.38): Removed unused `zeroize` derive feature.

Twenty-sixth audit (v0.0.39): Fixed 3 issues — concurrency limit included health/metrics endpoints, RUNBOOK referenced `curl` inside container, RUNBOOK secret rotation was non-atomic.

Twenty-seventh audit (v0.0.41): Comprehensive Opus-level audit of all source files, specs, K8s manifests, CI, Dockerfile, and RUNBOOK. Found 2 MEDIUM issues, 0 HIGH/LOW, 0 spec discrepancies. (1) K8s deployment.yaml missing pod-level `securityContext` with `seccompProfile: RuntimeDefault` — the restricted pod security profile requires this for admission controller compliance. Container-level security contexts were complete but pod-level was absent. Fixed by adding pod-level `securityContext` with `runAsNonRoot`, `seccompProfile.type: RuntimeDefault`, and `fsGroup: 1000`. (2) Proxy container image used mutable `:main` branch tag without documentation — operators may not realize rollbacks are non-deterministic. Added comment noting production should use SHA or semver tags. All 83 tests pass, clippy clean, formatting clean.

Twenty-eighth audit (v0.0.42): Comprehensive Opus-level audit of all source files, specs, K8s manifests, CI, Dockerfile, and RUNBOOK. Found 2 issues fixed, 0 HIGH. (1) State machine `unreachable!()` in production code path — the `Running + RequestReceived/RequestCompleted` match arm used `unreachable!()` which would abort the process (especially dangerous with `panic = "abort"` release profile) if accidentally triggered by future code. Replaced with a defensive no-op return. Updated spec to match. (2) K8s resources (namespace, configmap, serviceaccount, secret) missing `app: anthropic-oauth-proxy` labels — prevents `kubectl get -l app=...` queries from finding all project resources. Added labels to all four manifests. RUNBOOK, specs, and all other dimensions fully consistent. All 83 tests pass, clippy clean, formatting clean.

Twenty-ninth audit (v0.0.42): Comprehensive Opus-level audit of all source files, specs, K8s manifests, CI, Dockerfile, and RUNBOOK. 0 issues found across all nine dimensions: bugs, security, spec discrepancies, K8s manifests, Dockerfile, CI/CD, RUNBOOK accuracy, code quality, and documentation. All 83 tests pass, clippy clean, formatting clean. Codebase is fully consistent with specs. All remaining work requires live infrastructure.

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
- K8s `terminationGracePeriodSeconds` should be DRAIN_TIMEOUT + small buffer (e.g. 1s), not significantly larger. The application force-exits after DRAIN_TIMEOUT regardless, so the extra Kubernetes wait is wasted delay during rolling updates and node drains.
- Kustomize secrets with placeholder values overwrite real secrets on `kubectl apply -k`. If a secret contains a real credential created imperatively, do NOT include it in `kustomization.yaml`. Keep a schema-documenting `secret.yaml` in the repo but excluded from kustomization resources. The RUNBOOK should instruct users to create the secret imperatively after `kubectl apply -k`.
- K8s Pod Security Standards restricted profile requires `runAsNonRoot: true` on every container, not just the main application container. Setting `runAsUser: 1000` is not sufficient — the explicit `runAsNonRoot` field is what Kubernetes admission controllers check. Missing it on sidecar containers is easy to overlook.
- Prometheus histograms and summaries are different metric types with different semantics. Histograms produce `_bucket`, `_sum`, and `_count` lines; quantiles are computed at query time via `histogram_quantile()`. Summaries compute quantiles client-side. Documentation must use precise terminology — saying a histogram "automatically computes quantiles" is misleading and confuses operators writing PromQL.
- Undocumented environment variable overrides create debugging blind spots. If code reads an env var to override defaults (like `TAILSCALE_SOCKET` for the socket path), it must be documented in both the spec's environment variables table and the operational runbook's troubleshooting section.
- Crate `derive` features (e.g. `zeroize = { features = ["derive"] }`) pull in proc-macro dependencies (`syn`, `quote`, `proc-macro2`). Only enable them if `#[derive(Trait)]` is actually used. Using a trait as a bound or calling methods directly does not require the derive feature.
- Concurrency limits on a proxy must exclude observability endpoints. K8s liveness/readiness probes and Prometheus scrapes must always be responsive regardless of proxy load. In axum, use `Router::merge()` to nest a concurrency-limited sub-router (proxy routes) under an unlimited parent router (health/metrics routes).
- K8s secret rotation should use `kubectl create --dry-run=client -o yaml | kubectl apply -f -` for atomic updates. A `delete` then `create` sequence leaves a window where pods rescheduled between the two commands fail with `CreateContainerConfigError`.
- Minimal Docker images (debian-slim + ca-certificates only) don't have debugging tools like `curl`. RUNBOOK troubleshooting steps should use `kubectl port-forward` from the operator's workstation instead of `kubectl exec` with tools that aren't in the image.
- K8s restricted pod security profile requires pod-level `securityContext` with `seccompProfile.type: RuntimeDefault`. Container-level security contexts alone are insufficient — admission controllers check the pod-level seccomp profile separately. Also set `fsGroup` at the pod level so emptyDir volumes are writable by the non-root group.
- `unreachable!()` in non-test code paths is a latent process abort, especially with `panic = "abort"` in the release profile. Even if the current caller never triggers the arm, future code changes might. Replace `unreachable!()` with defensive no-op returns in state machines where the arm is theoretically reachable but practically unused.
- K8s resources should carry consistent `app:` labels even if they are not selected by anything. Labels enable `kubectl get <kind> -l app=<name>` queries for discovering all resources belonging to a project, which aids operational debugging and bulk cleanup.

## Environment Notes

- Rust toolchain: cargo 1.93.0, rustc 1.93.0 (stable, Jan 2026), installed at `~/.cargo/bin/cargo`
- Cross-compilation: `cargo-zigbuild` v0.21.6 + zig 0.15.2 for Linux targets
