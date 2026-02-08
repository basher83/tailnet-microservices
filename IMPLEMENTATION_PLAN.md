# Implementation Plan — Operator Migration

Previous build history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102, E2E verified 2026-02-06).

## Current Work

Executing specs/operator-migration.md — remove tailscaled sidecar, delegate tailnet exposure to Tailscale Operator.

## Status

Not started. Plan audited 2026-02-08 — all 53 task items verified against source code.

## Task List

Groups 1-4 are a single atomic code change — the codebase compiles after Group 4 is complete (service.rs types are referenced by error.rs, tailnet.rs, and main.rs, so all four must change together). Groups 5-11 are independently compilable after Group 4. Within groups, items are dependency-ordered.

### 1. State machine simplification (service.rs)

Must come first because main.rs, error.rs, and tailnet.rs all depend on types defined here. Changing the state machine establishes the new shape everything else conforms to.

- [ ] 1.1 Remove `ErrorOrigin` enum
- [ ] 1.2 Remove `TailnetHandle` struct
- [ ] 1.3 Remove `ConnectingTailnet` variant from `ServiceState`
- [ ] 1.4 Remove `tailnet: TailnetHandle` field from `Starting` variant (keep `listen_addr`)
- [ ] 1.5 Remove `tailnet: TailnetHandle` field from `Running` variant (keep `listen_addr`)
- [ ] 1.6 Remove `Error` variant from `ServiceState`
- [ ] 1.7 Remove `TailnetConnected(TailnetHandle)` from `ServiceEvent`
- [ ] 1.8 Remove `TailnetError(String)` from `ServiceEvent`
- [ ] 1.9 Remove `RetryTimer` from `ServiceEvent`
- [ ] 1.10 Remove `ConnectTailnet` from `ServiceAction`
- [ ] 1.11 Remove `ScheduleRetry { delay: Duration }` from `ServiceAction`
- [ ] 1.12 Remove `MAX_TAILNET_RETRIES` constant
- [ ] 1.13 Rewrite `handle_event`: `Initializing + ConfigLoaded` transitions directly to `Starting { listen_addr }` with `StartListener { addr }`. Remove all `ConnectingTailnet`, `TailnetConnected`, `TailnetError`, `RetryTimer`, and `Error` match arms
- [ ] 1.14 Delete tests: `dummy_tailnet_handle()`, `init_to_connecting_on_config_loaded`, `connecting_to_starting_on_tailnet_connected`, `connecting_error_triggers_retry_with_backoff`, `max_retries_stops_service`, `error_retry_timer_returns_to_connecting`, `connecting_error_backoff_values_match_spec`, `error_state_ignores_irrelevant_events`, `connecting_ignores_unexpected_events`, `shutdown_signal_from_error_stops`
- [ ] 1.15 Update remaining tests: remove `TailnetHandle` from `Starting`/`Running` constructors, remove `TailnetConnected`/`TailnetError`/`RetryTimer` events from terminal-state test event lists (`stopped_state_is_terminal`, `stopped_with_failure_exit_code_is_terminal`). Rewrite `non_default_listen_addr_preserved_through_transitions` for new flow (Initializing→Starting→Running, no ConnectingTailnet hop). Rewrite `any_state_shutdown_signal_stops` to use `Starting` instead of `ConnectingTailnet`. Update `initializing_ignores_unexpected_events` to remove `TailnetConnected` and `RetryTimer` sub-assertions. Update `starting_ignores_unexpected_events` to remove `TailnetError` and `RetryTimer` sub-assertions. Update `running_ignores_lifecycle_events` to remove `TailnetConnected`, `TailnetError`, `RetryTimer` from event list. Update `running_handles_request_events_as_noop` and `running_to_draining_on_shutdown` and `starting_to_running_on_listener_ready` and `shutdown_signal_from_starting_stops` to remove `TailnetHandle` from constructors
- [ ] 1.16 Add test: `init_to_starting_on_config_loaded` verifying `Initializing + ConfigLoaded -> Starting` with `StartListener`

### 2. Delete error.rs (service error types)

All 4 variants are tailnet-specific. With the state machine simplified, nothing constructs these errors. The `ServiceError` reference in `ServiceEvent::RequestCompleted` should be replaced with `Option<String>`.

- [ ] 2.1 In `service.rs`: remove `use crate::error::Error as ServiceError`, change `RequestCompleted.error` from `Option<ServiceError>` to `Option<String>`
- [ ] 2.2 Delete `services/oauth-proxy/src/error.rs`
- [ ] 2.3 Remove `mod error;` from `main.rs`

### 3. Delete tailnet.rs

With service.rs and error.rs no longer referencing tailnet types, this file is safely deletable.

- [ ] 3.1 Delete `services/oauth-proxy/src/tailnet.rs`
- [ ] 3.2 Remove `mod tailnet;` from `main.rs`

### 4. Rewrite main.rs

The main function, health handler, AppState, and tests all reference removed types.

- [ ] 4.1 Clean up imports: remove `TailnetHandle` from `use crate::service::{...}`
- [ ] 4.2 Remove `tailnet: Option<TailnetHandle>` from `AppState`
- [ ] 4.3 Rewrite `main()`: remove `config.tailscale.hostname` log, replace `ConnectingTailnet` transition with direct `Initializing + ConfigLoaded -> Starting`, delete entire tailnet connect loop, delete `set_tailnet_connected()` calls, remove `tailnet` from `AppState` construction
- [ ] 4.4 Rewrite `health_handler`: always return 200 with `{"status": "healthy", "uptime_seconds": N, "requests_served": N, "errors_total": N}` — no tailnet fields, no 503 branch
- [ ] 4.5 Remove `tailnet` field from all test helpers (`test_app_state()` and ~17 inline `AppState` constructions — compiler will catch all sites)
- [ ] 4.6 Update `health_endpoint_returns_json` test: remove tailnet field assertions
- [ ] 4.7 Delete `health_endpoint_without_tailnet_returns_not_connected` test (no degraded state)
- [ ] 4.8 Update `metrics_endpoint_contains_spec_metric_names_after_request` test: remove `set_tailnet_connected(true)` call (line 1246), remove `tailnet_connected` assertion (lines 1295-1298), update comment from "four spec-defined metric names" to "three" (line 1282)
- [ ] 4.9 Update module doc comment (remove "Joins tailnet with its own identity")

### 5. Metrics cleanup (metrics.rs)

- [ ] 5.1 Delete `set_tailnet_connected()` function
- [ ] 5.2 Delete `set_tailnet_connected_updates_gauge` test
- [ ] 5.3 Remove `set_tailnet_connected` calls from `record_functions_do_not_panic_without_recorder` test
- [ ] 5.4 Remove `tailnet_connected` from module doc comment

### 6. Configuration cleanup (config.rs)

- [ ] 6.1 Remove `use common::Secret;`
- [ ] 6.2 Delete `TailscaleConfig` struct
- [ ] 6.3 Remove `pub tailscale: TailscaleConfig` from `Config`
- [ ] 6.4 Remove auth key resolution logic from `Config::load()` (TS_AUTHKEY env var, auth_key_file)
- [ ] 6.5 Update `valid_toml()` test helper: remove `[tailscale]` section
- [ ] 6.6 Delete auth key tests: `test_auth_key_from_env`, `test_auth_key_from_file`, `test_auth_key_env_overrides_file`, `test_auth_key_file_empty_content_yields_none`, `test_auth_key_env_overrides_nonexistent_file`, `test_auth_key_file_nonexistent_returns_error`
- [ ] 6.7 Update `test_load_valid_config`: remove tailscale assertions
- [ ] 6.8 Update `test_max_connections_custom`: remove `[tailscale]` from inline TOML
- [ ] 6.9 Update validation tests (`test_invalid_upstream_url_rejected`, `test_zero_timeout_rejected`, `test_zero_max_connections_rejected`): remove `[tailscale]` from inline TOML
- [ ] 6.10 Update `test_missing_required_fields_returns_deserialization_error`: remove hostname sub-case, remove `[tailscale]` from remaining sub-cases
- [ ] 6.11 Update config.rs module doc comment (lines 1-5): remove auth_key reference ("The auth_key is loaded from TS_AUTHKEY env var or auth_key_file, never stored in the TOML directly to avoid leaking secrets.")

### 7. Common crate cleanup

`Secret<T>` is used exclusively by `TailscaleConfig.auth_key`. With that gone, `Secret`, `secret.rs`, and `zeroize` are dead code.

- [ ] 7.1 Delete `crates/common/src/secret.rs`
- [ ] 7.2 Remove `mod secret;` and `pub use secret::Secret;` from `crates/common/src/lib.rs`
- [ ] 7.3 Remove `zeroize` from `crates/common/Cargo.toml`
- [ ] 7.4 Remove `zeroize = "1"` from workspace `[workspace.dependencies]` in root `Cargo.toml`

### 8. Cargo dependency cleanup

- [ ] 8.1 Remove `tailscale-localapi = { workspace = true }` from `services/oauth-proxy/Cargo.toml`
- [ ] 8.2 Remove `tailscale-localapi = "0.4"` from workspace dependencies in root `Cargo.toml`
- [ ] 8.3 Remove `thiserror = { workspace = true }` from `services/oauth-proxy/Cargo.toml` (only consumer was `error.rs`, now deleted)

### 9. Kubernetes manifest updates

- [ ] 9.1 `k8s/deployment.yaml`: remove tailscaled sidecar container, `imagePullSecrets`, `TAILSCALE_SOCKET` env var, `tailscale-socket` volumeMount, `tailscale-state` and `tailscale-socket` volumes
- [ ] 9.2 `k8s/service.yaml`: add `tailscale.com/expose: "true"` and `tailscale.com/hostname: "anthropic-oauth-proxy"` annotations
- [ ] 9.3 `k8s/configmap.yaml`: remove `[tailscale]` section (hostname, state_dir)
- [ ] 9.4 Delete `k8s/secret.yaml`
- [ ] 9.5 `k8s/kustomization.yaml`: remove commented secret.yaml reference and imperative creation instructions

### 10. Example config and Dockerfile cleanup

- [ ] 10.1 `anthropic-oauth-proxy.example.toml`: remove `[tailscale]` section and auth key comment
- [ ] 10.2 `Dockerfile`: update comment from "requires a tailscaled sidecar" to standalone container note

### 11. Verification

- [ ] 11.1 `cargo fmt --all --check`
- [ ] 11.2 `cargo clippy --workspace -- -D warnings`
- [ ] 11.3 `cargo build --workspace`
- [ ] 11.4 `cargo test --workspace`

## Design Decisions

**Secret<T> and zeroize (Group 7):** `Secret` is used exclusively in `config.rs` for `TailscaleConfig.auth_key`. Once `TailscaleConfig` is deleted, nothing references `Secret`. The `zeroize` dependency exists solely to support it. Both removed.

**error.rs (Group 2):** All 4 variants (`TailnetAuth`, `TailnetMachineAuth`, `TailnetConnect`, `TailnetNotRunning`) are tailnet-specific. `ServiceEvent::RequestCompleted.error` field changes from `Option<ServiceError>` to `Option<String>` (never read by the state machine anyway). The file is deleted.

**Compilation order:** Groups 1-4 are tightly coupled — service.rs defines `TailnetHandle`, `ErrorOrigin`, and state/event/action variants that error.rs, tailnet.rs, and main.rs depend on. After Group 1 alone, `main.rs` and `tailnet.rs` fail to compile (they reference removed types). After Group 2, `main.rs` still references `crate::error` types. The ordering within this block (service.rs → error.rs → tailnet.rs → main.rs) is the logical dependency order, but all four groups must be completed as a single atomic change before the build passes. Groups 5-11 are independently compilable.

**Unaffected dependencies:** `serde_json` (used by proxy.rs, main.rs), `libc` in dev-deps (macOS memory tests), `uuid` (request IDs) — all stay.

**proxy.rs:** No changes (R4).

## Audit Log

**2026-02-08:** Full code audit. Every task item (53 total) verified against source code. All type names, test names, struct fields, enum variants, constants, imports, and file paths confirmed to exist exactly as described. Two items refined for explicitness: 4.5 scope clarified (~17 inline AppState constructions), 4.8 expanded with specific test name and line references. No missing items found. Plan is correct, complete, and properly ordered.
