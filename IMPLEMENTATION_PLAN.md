# Implementation Plan

Previous build history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102, E2E verified 2026-02-06).

## Status

All work complete. Operator migration deployed. Specs synchronized with codebase.

Verification: `cargo fmt --all --check` clean, `cargo clippy --workspace -- -D warnings` clean, `cargo build --workspace` clean, `cargo test --workspace` 86 passed (82 oauth-proxy + 4 common) / 2 ignored (load test, memory soak).

## Audit Log

**2026-02-08 (v0.0.112):** README.md post-migration sync. Opus audit across all 22 project files found the only remaining stale references were in README.md: (1) overview said services "join a Tailscale tailnet" — rewritten for Operator-delegated architecture, (2) service description referenced "tailscaled sidecar sharing a Unix socket" — corrected to single-container pod, (3) project structure listed `Secret<T>` in common crate — removed (type deleted during migration), (4) configuration section referenced `TS_AUTHKEY` — removed (no secrets required), (5) deployment section said "after creating the required secrets" — corrected. All Rust code, specs, K8s manifests, Dockerfile, CI, RUNBOOK, and example config verified fully synchronized. 86 tests pass (82 oauth-proxy + 4 common), 2 ignored (load test, memory soak). Pipeline clean.

**2026-02-08 (v0.0.111):** Exhaustive spec-vs-code audit (Opus). Every requirement in specs/oauth-proxy.md verified line-by-line against implementation: all types, state machine transitions, HTTP proxy protocol (header injection, authorization protection, host stripping, hop-by-hop stripping, body limit, response streaming, retry logic), error response format, configuration (TOML, precedence, defaults, fail-fast validation), observability (JSON logging, Prometheus metrics with correct names/labels/buckets, health/metrics endpoints outside concurrency limit), build (binary name, release profile, Dockerfile, CI), and K8s manifests (namespace, service annotations, deployment security context, probes, terminationGracePeriodSeconds). Zero gaps, zero bugs, zero deviations found. 86 tests pass (82 oauth-proxy + 4 common), 2 ignored (load test, memory soak). Pipeline clean.

**2026-02-08 (v0.0.110):** Config fail-fast validation. Opus audit found two gaps where misconfiguration was caught at runtime instead of startup: (1) `upstream_url` was only checked for `http://`/`https://` prefix but not parsed as a valid URL, so `"https://"` (no host) would pass config load and fail on first request; (2) header injection names/values were validated per-request with `warn!()` and skipped, so an operator could deploy invalid headers and only discover it via logs. Fixed both: `upstream_url` now parsed with `reqwest::Url::parse()` at load time, header names/values validated with `HeaderName::from_str()`/`HeaderValue::from_str()` at load time. Added 4 new tests: unparseable URL, non-http scheme, invalid header name, invalid header value with CRLF. 86 tests pass (82 + 4 common), 2 ignored.

**2026-02-08 (v0.0.109):** RUNBOOK.md rewrite. The runbook was dangerously stale post-operator-migration: described two-container sidecar architecture, referenced deleted secrets (tailscale-authkey, ghcr-pull-secret), defunct tailnet health fields, removed `tailnet_connected` metric, and eliminated error states (TailnetAuth, TailnetMachineAuth, TailnetNotRunning, TailnetConnect). Rewrote for single-container operator-delegated architecture. Added Tailscale Operator troubleshooting section. 82 tests pass (78 + 4 common), 2 ignored.

**2026-02-08 (v0.0.108):** Full spec-vs-code gap analysis (Opus audit). Found one stale error message in `main.rs:120` referencing `TailnetConnected` (pre-migration state) instead of `ConfigLoaded`. Fixed. All 60+ spec requirements verified against code — no other gaps. 78 tests pass, 2 ignored.

**2026-02-08 (v0.0.107):** Spec synchronization audit. Found `specs/oauth-proxy.md` dangerously stale post-migration (referenced deleted files: tailnet.rs, error.rs; deleted types: TailscaleConfig, Secret, TailnetHandle, ErrorOrigin; removed states: ConnectingTailnet, Error; removed config: [tailscale] section; removed env vars: TS_AUTHKEY, TAILSCALE_SOCKET; removed metric: tailnet_connected). Rewrote spec to match current codebase. Updated `specs/operator-migration.md` status from Draft to Complete, checked all 12 success criteria. Updated `specs/README.md` to reflect current status. Fixed `service.rs` import ordering (`cargo fmt`).

**2026-02-08:** All 53 operator migration items implemented. Files deleted: error.rs, tailnet.rs, secret.rs, k8s/secret.yaml. Files rewritten: service.rs, main.rs, metrics.rs, config.rs, common/lib.rs. Cargo deps cleaned: tailscale-localapi, zeroize, thiserror (from oauth-proxy). K8s manifests updated for operator pattern.
