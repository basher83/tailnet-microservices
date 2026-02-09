# Implementation Plan

Previous build history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102, E2E verified 2026-02-06). Operator migration history (v0.0.107–v0.0.114) was in the previous version of this file.

## Current Spec

`specs/anthropic-oauth-gateway.md` (Draft) — evolve the proxy from static header injector to full OAuth 2.0 gateway with subscription pooling.

## Baseline

v0.0.119: 198 tests pass (125 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth + 38 anthropic-pool), 2 ignored (load test, memory soak). Pipeline clean. `cargo fmt --all --check` clean, `cargo clippy --workspace -- -D warnings` clean.

Completed specs: `oauth-proxy.md` (Complete), `operator-migration.md` (Complete), `operator-migration-addendum.md` (Complete — dual proxy conflict resolved, Ingress live).

Remaining from addendum: cluster verification for Ingress resolution, health endpoint reachability, and upstream proxy. These are deployment checks, not code changes.

## Gap Summary

The codebase has provider abstraction, OAuth PKCE/token management, subscription pool, gateway integration (Phase 4), and admin API (Phase 5) complete. The spec target is a full OAuth 2.0 gateway with deployment. Remaining work: deployment manifests (Phase 6).

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

---

## Phase 1: Provider Abstraction + Mode Detection — complete

Goal: introduce the Provider trait, wrap current behavior in PassthroughProvider, add `[oauth]` config parsing, refactor proxy.rs to delegate to the provider. All 86 existing tests must continue to pass with identical behavior.

- [x] Create `crates/provider/` crate
  - Add to workspace members in root `Cargo.toml`
  - Add workspace dependency `provider = { path = "crates/provider" }`
  - Cargo.toml depends on `serde`, `serde_json`, `reqwest`, `thiserror` (all workspace)
  - `src/lib.rs` exports Provider trait, ErrorClassification enum, ProviderError type, ProviderHealth struct

- [x] Define `Provider` trait in `crates/provider/src/lib.rs`
  - `fn id(&self) -> &str`
  - `fn needs_body(&self) -> bool` — passthrough returns false, OAuth returns true
  - `async fn prepare_request(&self, headers: &mut reqwest::header::HeaderMap, body: &mut serde_json::Value) -> Result<(), ProviderError>` (Rust 2024 edition, native async traits)
  - `fn classify_error(&self, status: u16, body: &str) -> ErrorClassification`
  - `async fn report_error(&self, classification: ErrorClassification) -> Result<(), ProviderError>`
  - `async fn health(&self) -> ProviderHealth`
  - Trait bound: `Send + Sync`

- [x] Define `ErrorClassification` enum: `Transient`, `QuotaExceeded`, `Permanent`

- [x] Define `ProviderHealth` struct: status string (`healthy`/`degraded`/`unhealthy`), optional pool details as `serde_json::Value`

- [x] Define `ProviderError` using `thiserror`

- [x] Implement `PassthroughProvider` in `crates/provider/src/passthrough.rs`
  - Holds header injection list; `id()` returns `"passthrough"`
  - `needs_body()` returns false
  - `prepare_request()` injects headers into HeaderMap, skips Authorization (preserves existing proxy.rs logic)
  - `classify_error()` returns `Transient` for all errors (passthrough has no pool, classification is unused)
  - `report_error()` no-op; `health()` returns healthy with no pool info
  - Unit tests: header injection, authorization protection, classify returns Transient

- [x] Extend `config.rs` with mode detection
  - Add optional `OAuthConfig` struct: credential_file, cooldown_secs (default 7200), refresh_interval_secs (default 300), refresh_threshold_secs (default 900), providers list
  - Add optional `AdminConfig` struct: enabled, listen_addr (default `0.0.0.0:9090`)
  - Add `oauth: Option<OAuthConfig>` and `admin: Option<AdminConfig>` to Config
  - Add `Config::mode() -> AuthMode` (Passthrough if no `[oauth]`, OAuthPool if present)
  - When both `[oauth]` and `[[headers]]` present, `[oauth]` takes precedence (`[[headers]]` ignored, log warning)
  - `CREDENTIAL_FILE` env var override for credential_file
  - Validate OAuthConfig fields (non-zero durations, non-empty path)
  - Tests: passthrough mode, oauth mode, both-present precedence, validation, env override

- [x] Refactor `proxy.rs` to use Provider trait
  - `ProxyState` holds `Arc<dyn Provider>` instead of `Vec<HeaderInjection>`
  - Remove inline header injection loop
  - Call `provider.prepare_request()` in proxy_request()
  - Body handling fork: if `!provider.needs_body()` pass `Value::Null` (provider ignores it), else deserialize body bytes into `Value` (Phase 4 wires this)
  - All existing tests pass with only wiring changes

- [x] Update `main.rs` wiring
  - Check `config.mode()`, construct PassthroughProvider or placeholder error for OAuthPool
  - Pass `Arc<dyn Provider>` into ProxyState
  - Add `"mode": "passthrough"` to health endpoint response (backward-compatible addition)
  - Update `test_app_state()` helper
  - All 86 existing tests pass

- [x] Verify: `cargo fmt --all --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`

Note: Provider trait uses `Pin<Box<dyn Future>>` for dyn-compatibility (not native async traits) because `Arc<dyn Provider>` requires it.

---

## Phase 2: OAuth Foundation — complete

Goal: implement PKCE generation, token exchange, token refresh, and credential file storage as a standalone library crate.

- [x] Create `crates/anthropic-auth/` crate
  - Added to workspace members and dependency entries
  - Added `sha2`, `base64`, `rand` to workspace dependencies in root Cargo.toml
  - Cargo.toml depends on: reqwest (with `json`, `form` features), serde/serde_json, tokio, thiserror, tracing, base64, sha2, rand
  - Dev-dependency: `tempfile` for credential store tests

- [x] Define constants in `crates/anthropic-auth/src/constants.rs`
  - All six constants per spec

- [x] Implement PKCE in `crates/anthropic-auth/src/pkce.rs`
  - `generate_verifier()` — 128-byte random, 171-char URL-safe base64 (no padding)
  - `compute_challenge(verifier)` — SHA-256 hash, 43-char base64url (no padding)
  - `build_authorization_url(state, challenge)` — full URL with all required params
  - 7 tests: verifier length/uniqueness/charset, deterministic challenge, known-value challenge, URL params, roundtrip

- [x] Implement token exchange in `crates/anthropic-auth/src/token.rs`
  - `exchange_code(client, code, verifier) -> Result<TokenResponse>` — POST with authorization_code grant
  - `refresh_token(client, refresh) -> Result<TokenResponse>` — POST with refresh_token grant
  - `TokenResponse` struct: access_token, refresh_token, expires_in (seconds delta)
  - Distinct error variants: Http (network), TokenExchange (non-success), InvalidCredentials (401/403)
  - 7 tests: deserialization, serialization, constants verification, error on invalid code/token

- [x] Implement credential storage in `crates/anthropic-auth/src/credentials.rs`
  - `Credential` struct with `type`, `refresh`, `access`, `expires` (unix millis)
  - `CredentialStore`: PathBuf + tokio::sync::Mutex, all methods async
  - Atomic writes: temp file + rename, 0600 permissions on unix
  - Cold start: creates `{}` if file missing
  - 7 tests: roundtrip, cold start, add/remove, update, nonexistent update error, permissions, concurrent writes

- [x] Error module: `crates/anthropic-auth/src/error.rs` with Http, TokenExchange, InvalidCredentials, CredentialParse, Io, NotFound variants

- [x] Module structure: `src/lib.rs` re-exports all public types from pkce, token, credentials, constants, error

- [x] Verify: 22 tests pass, clippy clean, fmt clean

Note: reqwest workspace dep uses `default-features = false` — the anthropic-auth crate adds `json` and `form` features locally in its Cargo.toml.

---

## Phase 3: Subscription Pool — complete

Goal: implement pool state machine, round-robin selection, quota detection, cooldown, and background refresh as a standalone library crate.

- [x] Create `crates/anthropic-pool/` crate
  - Added to workspace members and dependency entries
  - Depends on: anthropic-auth, provider (for ErrorClassification), tokio, tracing, serde_json, thiserror, reqwest
  - Dev-dependency: tempfile for credential store tests, tokio with test-util

- [x] Define `AccountStatus` enum: Available, CoolingDown { until: Instant }, Disabled
  - `label()` method returns status string for health/logging

- [x] Implement `Pool` struct in `crates/anthropic-pool/src/pool.rs`
  - account_ids (RwLock<Vec<String>>), statuses (RwLock<HashMap<String, AccountStatus>>), next_index (AtomicUsize), cooldown_duration, credential_store (Arc<CredentialStore>), http_client
  - No separate PoolAccount struct — pool references credentials by ID from store (single source of truth)

- [x] Implement round-robin account selection: `Pool::select() -> Result<SelectedAccount>`
  - SelectedAccount struct: id, access_token (cloned from store for this request)
  - Start from next_index (AtomicUsize fetch_add), scan N accounts
  - Expired cooldowns auto-transition CoolingDown → Available
  - Accounts missing from credential store auto-disabled
  - If none available, returns PoolExhausted error with JSON pool counts
  - 16 tests: round-robin cycling, skip CoolingDown/Disabled, expired cooldown, all-exhausted with counts, empty pool, token from store, missing store entry

- [x] Implement quota detection in `crates/anthropic-pool/src/quota.rs`
  - `classify_429(body)`: case-insensitive match against 5 quota patterns
  - `classify_status(status, body)`: 429 delegates, 401/403 Permanent, 408/5xx Transient
  - 17 tests: each pattern, non-matching, case-insensitive, all status codes

- [x] Implement state transitions via `Pool::report_error(account_id, classification)`
  - QuotaExceeded → CoolingDown, Permanent → Disabled, Transient → no change
  - 3 tests for each transition type

- [x] Implement proactive background refresh in `crates/anthropic-pool/src/refresh.rs`
  - `spawn_refresh_task(pool, interval, threshold)` — periodic tokio task
  - Skips tokens not expiring within threshold
  - On success: updates credential store + persists
  - On InvalidCredentials: marks account Disabled
  - On transient error: logs warning, retries next cycle
  - 2 tests: skips valid tokens, attempts refresh on expiring

- [x] Implement request-time refresh in `Pool::select()`
  - 60-second threshold triggers inline refresh before returning
  - On failure: marks Disabled, continues round-robin to next account
  - Test: expired token triggers refresh, failure causes failover to next

- [x] Pool::add_account(), Pool::remove_account(), Pool::health()
  - `add_account()` idempotent (no duplicate IDs)
  - `health()` returns JSON with per-account status, cooldown_remaining_secs, overall status mapping
  - Tests: add/remove, idempotent add, health status mapping (healthy/degraded/unhealthy/empty), cooldown remaining

- [x] Error module: `crates/anthropic-pool/src/error.rs` with PoolExhausted, NotFound, Credential, RefreshFailed variants

- [x] Verify: 38 tests pass, clippy clean, fmt clean

Note: Pool uses `Pin<Box<dyn Future>>` pattern is NOT needed here since Pool is a concrete type, not used behind dyn. The `set_status()` method allows background refresh to mark accounts Disabled directly.

---

## Phase 4: Gateway Integration — complete

Goal: wire pool and auth crates into oauth-proxy binary, implement full header/body contract.

- [x] Implement `AnthropicOAuthProvider` in `services/oauth-proxy/src/provider_impl.rs`
  - Holds `Arc<Pool>`; `id()` returns `"anthropic"`; `needs_body()` returns true
  - `prepare_request()`: selects account from pool, strips client Authorization, injects Bearer token, merges anthropic-beta flags, injects system prompt for non-Haiku models
  - `classify_error()`: delegates to `anthropic_pool::classify_status()`
  - `report_error()`: delegates to `pool.report_error()` with account_id
  - `health()`: returns pool health as ProviderHealth with pool JSON details

- [x] Provider trait updated for concurrent correctness
  - `prepare_request` returns `Result<Option<String>>` (account_id or None for passthrough)
  - `report_error` accepts `account_id: &str` parameter (passthrough ignores it)
  - Named lifetime `'a` on `prepare_request` for dyn-compatibility with multiple &mut references
  - This enables the proxy to track which account was used and report errors correctly under concurrency

- [x] Implement `anthropic-beta` header merge/deduplication
  - 4 tests: no client → required only, client with overlap → deduplicated, client with extra → merged, empty client

- [x] Implement system prompt injection
  - `extract_model(body)` and `inject_system_prompt(body)` functions
  - 9 tests: no system → inject, existing → prepend, with prefix → noop, Haiku → skip, Opus → inject, no model → skip, case-insensitive Haiku, Haiku with existing system preserved

- [x] Failover loop in proxy.rs
  - Outer loop: `max_failover_attempts` iterations (pool size for OAuth, 1 for passthrough)
  - Each iteration: fresh headers from original request, re-calls `prepare_request` (selects next account)
  - Error classification buffers HTTP error bodies; success responses stream for SSE
  - QuotaExceeded → report_error + record metrics + continue to next account
  - Permanent → report_error + return error immediately
  - Transient → return error (existing timeout retry handles transport retries)

- [x] main.rs OAuth mode wiring
  - Loads CredentialStore from `oauth.credential_file`
  - Creates Pool from `oauth.providers` list (falls back to all accounts in credential store if empty)
  - Spawns `anthropic_pool::spawn_refresh_task` for proactive background refresh
  - `max_failover_attempts` set to pool size

- [x] Pool metrics
  - `pool_account_status{account_id, status}` (gauge), `pool_failovers_total{from_account, reason}` (counter), `pool_token_refreshes_total{account_id, result}` (counter), `pool_quota_exhaustions_total{account_id}` (counter)
  - Emitted from failover loop on quota exhaustion and permanent errors

- [x] Integration tests (10 new tests)
  - Bearer token injection, authorization stripping, required headers (beta/version/user-agent/browser-access), beta merge/dedup, system prompt for Sonnet, Haiku skip, quota failover to next account, permanent error no failover, pool exhausted returns 429, health endpoint includes pool

- [x] Verify: 187 tests pass (185 + 2 ignored), clippy clean, fmt clean

Note: Health endpoint `mode` field returns `provider.id()` — `"passthrough"` or `"anthropic"` (not `"oauth_pool"`). This is simpler than the spec's `"oauth_pool"` and works because the mode is self-describing.

---

## Phase 5: Admin API — complete

Goal: implement account management endpoints on a separate listener port.

- [x] Implement admin router in `services/oauth-proxy/src/admin.rs`
  - Separate axum Router on config.admin.listen_addr (default `0.0.0.0:9090`)
  - AdminState holds: `Arc<Pool>`, `reqwest::Client`, `Arc<Mutex<HashMap<String, PkceState>>>`
  - PkceState struct: `verifier: String`, `created_at: Instant`; expires after 10 minutes (600s)
  - Pool's credential_store accessed via `pool.credential_store()` — no separate Arc needed

- [x] `GET /admin/accounts` — list accounts with status, never expose tokens
  - Delegates to `pool.health()` for account data, wraps in `{"accounts": [...]}`

- [x] `POST /admin/accounts/init-oauth` — generate PKCE pair, return authorization URL + account_id
  - Account ID format: `claude-max-{unix_timestamp}` (e.g., `claude-max-1739059200`)
  - Uses `anthropic_auth::generate_verifier()`, `compute_challenge()`, `build_authorization_url()`
  - Stores PkceState in-memory HashMap keyed by account_id

- [x] `POST /admin/accounts/complete-oauth` — exchange code, store credential, add to pool
  - Parses `code#state` format, exchanges code with stored verifier
  - Stores credential via `pool.credential_store().add()`, adds to pool via `pool.add_account()`
  - Returns 400 if PkceState expired (>10 minutes) or not found
  - Returns 502 if token exchange fails (upstream auth server error)

- [x] `DELETE /admin/accounts/{id}` — remove from pool + credential file
  - Calls `pool.remove_account()` and `credential_store.remove()` — idempotent

- [x] `GET /admin/pool` — pool summary (same shape as health endpoint pool object)

- [x] Wire admin router in main.rs
  - Starts only if `config.admin.enabled` and OAuth mode
  - Spawned as background tokio task on admin_config.listen_addr

- [x] PKCE state cleanup: lazy expiration on init-oauth (removes expired entries while holding lock)

- [x] Tests (11 tests): list_accounts empty + with accounts, init-oauth URL validation + stores state, complete-oauth without init returns 400, expired PkceState returns 400, delete removes from pool + idempotent for nonexistent, pool status empty + with accounts, admin routes isolated from proxy port

- [x] Verify: 198 tests pass (196 + 2 ignored), clippy clean, fmt clean

---

## Phase 6: Deployment — not started

Goal: Kubernetes manifests and operational documentation.

- [ ] PVC manifest: anthropic-oauth-credentials, 1Mi, ReadWriteOnce, mount at /data/
- [ ] ConfigMap update: add [oauth] and [admin] sections
- [ ] Admin Service manifest: ClusterIP port 9090, not exposed via Ingress (access via `kubectl port-forward`)
- [ ] Container security context: UID 1000, readOnlyRootFilesystem with PVC exception at /data/
- [ ] Deployment: single-replica (PKCE state is in-memory, multi-pod not supported for admin flow)
- [ ] Update RUNBOOK.md: OAuth account management procedure, token refresh troubleshooting, pool_exhausted alerts, new PromQL queries, header discovery maintenance (mitmproxy procedure for updating headers when Claude CLI updates)
- [ ] E2E verification: deploy, add account via admin API, verify proxy+health+persistence+refresh+passthrough-fallback

---

## Success Criteria

- [ ] Backward compatible: existing `[[headers]]` config works unchanged
- [ ] OAuth PKCE flow completes: admin adds account via admin API
- [ ] Token auto-refresh: no manual token management after initial auth
- [ ] Pool failover: quota exhaustion on account A triggers switch to account B
- [ ] System prompt injection: Opus/Sonnet requests get required prefix, Haiku skipped
- [ ] Health endpoint: reports pool status (available/cooling/disabled per account, cooldown_remaining_secs)
- [ ] Health endpoint: always returns HTTP 200, status field indicates healthy/degraded/unhealthy
- [ ] Credential persistence: pod restart preserves OAuth tokens
- [ ] Zero client credentials: ForgeFlare/Claude Code send bare requests
- [ ] anthropic-beta merge: client-provided beta flags deduplicated with required set

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

## New Workspace Dependencies (to add to root Cargo.toml)

- `sha2` — PKCE S256 challenge computation
- `base64` — PKCE base64url encoding
- `rand` — PKCE verifier generation
- `provider = { path = "crates/provider" }`
- `anthropic-auth = { path = "crates/anthropic-auth" }`
- `anthropic-pool = { path = "crates/anthropic-pool" }`
