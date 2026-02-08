# Implementation Plan — Operator Migration

Previous build history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102, E2E verified 2026-02-06).

## Current Work

Executing specs/operator-migration.md — remove tailscaled sidecar, delegate tailnet exposure to Tailscale Operator.

## Status

Complete. All 53 task items implemented 2026-02-08. Verification: `cargo fmt --all --check` clean, `cargo clippy --workspace -- -D warnings` clean, `cargo build --workspace` clean, `cargo test --workspace` 78 passed / 2 ignored (load test, memory soak).

## Completed Tasks

All groups (1-11) implemented as a single atomic change:

- Groups 1-4 (atomic): State machine simplified (removed `ErrorOrigin`, `TailnetHandle`, `ConnectingTailnet`, `Error` state, tailnet events/actions, retry logic). error.rs and tailnet.rs deleted. main.rs rewritten (direct Initializing->Starting flow, health returns 200 always, no tailnet fields). All tests updated.
- Group 5: `set_tailnet_connected()` and `tailnet_connected` gauge removed from metrics.rs.
- Group 6: `TailscaleConfig`, `Secret` import, auth key resolution, and 6 auth key tests removed from config.rs. All remaining tests updated to use simplified TOML without `[tailscale]` section.
- Group 7: `secret.rs` deleted, `Secret` and `zeroize` removed from common crate.
- Group 8: `tailscale-localapi`, `zeroize`, and `thiserror` (from oauth-proxy) removed from Cargo dependencies.
- Group 9: Deployment simplified to single container (no sidecar, no imagePullSecrets, no shared volumes). Service annotated with `tailscale.com/expose` and `tailscale.com/hostname`. ConfigMap and kustomization updated. secret.yaml deleted.
- Group 10: Example config and Dockerfile updated for standalone operation.
- Group 11: Full verification passed.

## Design Decisions

**Secret and zeroize:** `Secret` was used exclusively for `TailscaleConfig.auth_key`. With `TailscaleConfig` deleted, `Secret`, `secret.rs`, and `zeroize` are dead code. Removed.

**error.rs:** All 4 variants were tailnet-specific. `ServiceEvent::RequestCompleted.error` changed from `Option<ServiceError>` to `Option<String>`. File deleted.

**proxy.rs:** No changes (R4 — core proxy functionality unchanged).

**Unaffected dependencies:** `serde_json`, `libc` (dev-deps), `uuid` — all stay.

## Audit Log

**2026-02-08:** Full code audit. All 53 task items verified against source code. Plan confirmed correct and properly ordered.

**2026-02-08:** All 53 items implemented. Verification: fmt clean, clippy clean, build clean, 78 tests pass, 2 ignored (load test, memory soak). Files deleted: error.rs, tailnet.rs, secret.rs, k8s/secret.yaml. Files rewritten: service.rs, main.rs, metrics.rs, config.rs, common/lib.rs. Cargo deps cleaned: tailscale-localapi, zeroize, thiserror (from oauth-proxy). K8s manifests updated for operator pattern.
