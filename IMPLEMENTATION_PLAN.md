# Implementation Plan

Previous build history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102, E2E verified 2026-02-06). Operator migration history (v0.0.107–v0.0.114) was in the previous version of this file. OAuth gateway Phases 1–5 history was in the previous version of this file (v0.0.119).

## Current Spec

`specs/anthropic-oauth-gateway.md` (Draft) — evolve the proxy from static header injector to full OAuth 2.0 gateway with subscription pooling.

## Baseline

v0.0.120: 198 tests pass (125 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth + 38 anthropic-pool), 2 ignored (load test, memory soak). Pipeline clean. `cargo fmt --all --check` clean, `cargo clippy --workspace -- -D warnings` clean. `kubectl kustomize k8s/` validates clean.

Completed specs: `oauth-proxy.md` (Complete), `operator-migration.md` (Complete), `operator-migration-addendum.md` (Complete — dual proxy conflict resolved, Ingress live), `anthropic-oauth-gateway.md` (Phases 1–6 complete).

## All Phases Complete

The full OAuth 2.0 gateway implementation is code-complete across all six phases. Remaining work is deployment verification (cluster-side checks that require a running cluster).

### Verified Code State (2026-02-08)

- `crates/common/` — Error types only (`Config`, `Io`, `Toml` variants). 4 tests.
- `crates/provider/` — Provider trait (with `prepare_request` returning `Option<String>` account_id, and `report_error` accepting `account_id: &str`), ErrorClassification, ProviderError, ProviderHealth. PassthroughProvider wraps header injection. Uses named lifetime `'a` on `prepare_request` for dyn-compatibility with multiple mutable references. 9 tests.
- `services/oauth-proxy/src/config.rs` — `[proxy]`, `[[headers]]`, optional `[oauth]`, optional `[admin]`. AuthMode detection. OAuthConfig/AdminConfig structs with validation.
- `services/oauth-proxy/src/proxy.rs` — Failover loop: outer loop iterates accounts (capped at `max_failover_attempts`), inner loop handles timeout retries. Error classification buffers HTTP error response bodies for quota/permanent detection. Streams success responses for SSE. `build_buffered_response` and `build_streaming_response` helpers.
- `services/oauth-proxy/src/provider_impl.rs` — `AnthropicOAuthProvider` implementing Provider trait. Bearer token injection, `anthropic-beta` merge/dedup, system prompt injection (Opus/Sonnet get prefix, Haiku skipped), `anthropic-dangerous-direct-browser-access`, `user-agent`, `anthropic-version` headers. Delegates classify/report to pool. 13 unit tests.
- `services/oauth-proxy/src/main.rs` — OAuth mode wiring: loads CredentialStore, creates Pool from config providers (falls back to all credential store accounts), spawns background refresh task, creates AnthropicOAuthProvider. Spawns admin API listener on separate port when `admin.enabled` and OAuth mode. Health endpoint includes pool details in OAuth mode. 10 OAuth integration tests.
- `services/oauth-proxy/src/admin.rs` — Admin API on separate listener (default :9090). AdminState holds `Arc<Pool>`, `reqwest::Client`, `Arc<Mutex<HashMap<String, PkceState>>>`. Five endpoints: GET /admin/accounts (list with status, no tokens), POST /admin/accounts/init-oauth (PKCE flow initiation, account ID `claude-max-{timestamp}`), POST /admin/accounts/complete-oauth (code exchange, credential storage, pool addition), DELETE /admin/accounts/{id} (pool + credential removal), GET /admin/pool (pool health summary). Lazy PKCE state cleanup on init-oauth. 11 tests.
- `services/oauth-proxy/src/metrics.rs` — Seven metrics: `proxy_requests_total`, `proxy_request_duration_seconds`, `proxy_upstream_errors_total` (existing), plus `pool_account_status` (gauge), `pool_failovers_total`, `pool_token_refreshes_total`, `pool_quota_exhaustions_total` (new).
- `services/oauth-proxy/src/service.rs` — State machine: Initializing → Starting → Running → Draining → Stopped. `#[allow(dead_code)]` on state/event/action enums (spec-defined variants used only in tests).
- `crates/anthropic-auth/` — PKCE (generate_verifier, compute_challenge, build_authorization_url), token exchange/refresh, credential store (atomic writes, 0600 perms, Mutex-serialized). 22 tests.
- `crates/anthropic-pool/` — Pool state machine (AccountStatus: Available/CoolingDown/Disabled), round-robin selection with expired-cooldown auto-transition, quota detection (classify_429/classify_status), request-time inline refresh (60s threshold), background proactive refresh (spawn_refresh_task), pool management (add/remove/health). 38 tests.
- No TODO, FIXME, todo!(), or unimplemented!() anywhere in the codebase.
- Single `unreachable!()` in proxy.rs failover loop (defensive, correct).

### Verified Deployment State (2026-02-08)

- `k8s/pvc.yaml` — PersistentVolumeClaim `anthropic-oauth-credentials`, 1Mi, ReadWriteOnce. Mounted at `/data/` for credential file persistence across pod restarts.
- `k8s/configmap.yaml` — TOML config with `[proxy]` + `[[headers]]` (passthrough default). Commented-out `[oauth]` and `[admin]` sections ready to uncomment for OAuth mode.
- `k8s/admin-service.yaml` — ClusterIP Service on port 9090, not exposed via Ingress. Access via `kubectl port-forward`.
- `k8s/deployment.yaml` — Single-replica, UID 1000, readOnlyRootFilesystem with PVC exception at `/data/`. Ports: 8080 (http proxy), 9090 (admin API). Both config and credentials volumes mounted.
- `k8s/kustomization.yaml` — All 8 resources: namespace, serviceaccount, configmap, pvc, deployment, service, admin-service, ingress.
- `Dockerfile` — Exposes ports 8080 and 9090.
- `RUNBOOK.md` — Complete operational guide covering both passthrough and OAuth modes: deployment, OAuth account management (PKCE flow), monitoring (7 metrics + 4 PromQL alerts), token refresh troubleshooting, pool exhaustion recovery, header discovery maintenance.

---

## Remaining: E2E Cluster Verification

These are deployment checks requiring a running cluster, not code changes:

- [ ] Deploy updated manifests to cluster
- [ ] Verify PVC is bound and credential file writable
- [ ] Switch to OAuth mode (uncomment [oauth] in ConfigMap, restart)
- [ ] Add account via admin API PKCE flow (port-forward to 9090)
- [ ] Verify proxy request with OAuth account succeeds
- [ ] Verify health endpoint reports pool status
- [ ] Verify credential persistence across pod restart
- [ ] Verify background token refresh (check logs after 5+ minutes)
- [ ] Verify passthrough fallback (revert ConfigMap, restart)
- [ ] Verify only one Tailscale proxy pod exists (no dual-proxy)
- [ ] Verify Ingress resolves from tailnet (MagicDNS)

---

## Success Criteria

- [x] Backward compatible: existing `[[headers]]` config works unchanged
- [x] OAuth PKCE flow completes: admin adds account via admin API
- [x] Token auto-refresh: no manual token management after initial auth
- [x] Pool failover: quota exhaustion on account A triggers switch to account B
- [x] System prompt injection: Opus/Sonnet requests get required prefix, Haiku skipped
- [x] Health endpoint: reports pool status (available/cooling/disabled per account, cooldown_remaining_secs)
- [x] Health endpoint: always returns HTTP 200, status field indicates healthy/degraded/unhealthy
- [x] Credential persistence: pod restart preserves OAuth tokens (PVC mounted)
- [x] Zero client credentials: ForgeFlare/Claude Code send bare requests
- [x] anthropic-beta merge: client-provided beta flags deduplicated with required set

## Target Crate Dependency Graph

```text
oauth-proxy (binary)
  ├── common
  ├── provider          (Provider trait, ErrorClassification, PassthroughProvider)
  ├── anthropic-auth    (PKCE, token exchange/refresh, credential storage)
  └── anthropic-pool    (AccountStatus state machine, round-robin, failover, background refresh)
        ├── anthropic-auth
        └── provider    (ErrorClassification reuse)
```
