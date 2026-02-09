# Implementation Plan

Previous build history archived at IMPLEMENTATION_PLAN_v1.md (81 audits, 111 tests, v0.0.102, E2E verified 2026-02-06). Operator migration history (v0.0.107–v0.0.114) was in the previous version of this file.

## Current Spec

`specs/anthropic-oauth-gateway.md` (Draft) — evolve the proxy from static header injector to full OAuth 2.0 gateway with subscription pooling.

## Baseline

v0.0.116: 124 tests pass (89 oauth-proxy + 4 common + 9 provider + 22 anthropic-auth), 2 ignored (load test, memory soak). Pipeline clean. `cargo fmt --all --check` clean, `cargo clippy --workspace -- -D warnings` clean.

Completed specs: `oauth-proxy.md` (Complete), `operator-migration.md` (Complete), `operator-migration-addendum.md` (Complete — dual proxy conflict resolved, Ingress live).

Remaining from addendum: cluster verification for Ingress resolution, health endpoint reachability, and upstream proxy. These are deployment checks, not code changes.

## Gap Summary

The current codebase is a passthrough header injector with provider abstraction. The spec target is a full OAuth 2.0 gateway with PKCE auth, token lifecycle, subscription pooling, admin API, and body modification. Zero OAuth code exists today. The Provider trait and PassthroughProvider exist, but no `anthropic-auth` or `anthropic-pool` crates. The following phases track the spec's six-phase structure; each phase builds on the previous.

### Verified Code State (2026-02-08)

- `crates/common/` — Error types only (`Config`, `Io`, `Toml` variants). 4 tests.
- `crates/provider/` — Provider trait, ErrorClassification, ProviderError, ProviderHealth. PassthroughProvider wraps header injection. 9 tests.
- `services/oauth-proxy/src/config.rs` — `[proxy]`, `[[headers]]`, optional `[oauth]`, optional `[admin]`. AuthMode detection. OAuthConfig/AdminConfig structs with validation.
- `services/oauth-proxy/src/proxy.rs` — Provider trait delegation via `Arc<dyn Provider>`. Body handling fork (needs_body). No inline header injection.
- `services/oauth-proxy/src/main.rs` — Health returns `{status, mode, uptime_seconds, requests_served, errors_total}`. Mode comes from provider.id().
- `services/oauth-proxy/src/metrics.rs` — Three metrics only: `proxy_requests_total`, `proxy_request_duration_seconds`, `proxy_upstream_errors_total`. No pool metrics.
- `services/oauth-proxy/src/service.rs` — State machine: Initializing → Starting → Running → Draining → Stopped. `#[allow(dead_code)]` on state/event/action enums (spec-defined variants used only in tests).
- `crates/anthropic-auth/` — PKCE (generate_verifier, compute_challenge, build_authorization_url), token exchange/refresh, credential store (atomic writes, 0600 perms, Mutex-serialized). 22 tests.
- No `crates/anthropic-pool/` directory.
- No `admin.rs`, `provider_impl.rs` files in oauth-proxy.
- No TODO, FIXME, todo!(), or unimplemented!() anywhere in the codebase.
- Single `unreachable!()` in proxy.rs retry loop (defensive, correct).

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

## Phase 3: Subscription Pool — not started

Goal: implement pool state machine, round-robin selection, quota detection, cooldown, and background refresh as a standalone library crate.

- [ ] Create `crates/anthropic-pool/` crate
  - Add to workspace members and dependency entries
  - Depends on: anthropic-auth, provider (for ErrorClassification), tokio, tracing, serde_json, thiserror, reqwest

- [ ] Define `AccountStatus` enum: Available, CoolingDown { until: Instant }, Disabled

- [ ] Define `PoolAccount` struct
  - id (String), status (AccountStatus)
  - Credential data (access_token, refresh_token, expires_at) lives in CredentialStore; pool references by id
  - Pool reads credentials from store at selection time (single source of truth)

- [ ] Implement `Pool` struct
  - account_ids (RwLock<Vec<String>>), statuses (RwLock<HashMap<String, AccountStatus>>), next_index (AtomicUsize), cooldown_duration, credential_store (Arc<CredentialStore>), http_client

- [ ] Implement round-robin account selection: `Pool::select() -> Result<SelectedAccount>`
  - `SelectedAccount` struct: id, access_token (cloned from store for this request)
  - Start from next_index, scan N accounts, check expired cooldowns (transition CoolingDown → Available), return first Available
  - If none available, return error with pool counts for 503 response: `{"error": {"type": "pool_exhausted", "message": "All accounts exhausted", "pool": {"accounts_total": N, "available": 0, "cooling_down": X, "disabled": Y}}}`
  - Tests: round-robin cycling, skip CoolingDown/Disabled, expired cooldown transitions, all-exhausted error with correct JSON shape

- [ ] Implement quota detection in `crates/anthropic-pool/src/quota.rs`
  - `classify_429(body)`: QuotaExceeded for "5-hour", "5 hour", "rolling window", "usage limit for your plan", "subscription usage limit"; Transient otherwise
  - `classify_status(status, body)`: 429 delegates to classify_429, 401/403 Permanent, 408/5xx Transient
  - Tests: each pattern, non-matching 429, 401/403, 5xx

- [ ] Implement state transitions via `Pool::report_error(account_id, classification)`
  - QuotaExceeded → CoolingDown { until: now + cooldown_duration }, Permanent → Disabled, Transient → no change
  - Tests: all transitions including CoolingDown + refresh failure → Disabled

- [ ] Implement proactive background refresh
  - `spawn_refresh_task(pool, interval, threshold)` — periodic task refreshing expiring tokens
  - Check interval = config.refresh_interval_secs (default 300s), threshold = config.refresh_threshold_secs (default 900s)
  - On success: update token in credential store + persist; on 401/403: mark Disabled; on transient: log warning
  - Tests: mock token endpoint, verify refresh called, verify Disabled on 401

- [ ] Implement request-time refresh in `Pool::select()`
  - If selected account's token expires within 60s, attempt inline refresh before returning
  - On failure: mark Disabled, continue round-robin to next account
  - Tests: expiring token triggers refresh, failure causes failover

- [ ] Pool::add_account(), Pool::remove_account(), Pool::health()
  - `health()` returns per-account status list including `cooldown_remaining_secs` for CoolingDown accounts

- [ ] Verify: `cargo test -p anthropic-pool`

---

## Phase 4: Gateway Integration — not started

Goal: wire pool and auth crates into oauth-proxy binary, implement full header/body contract.

- [ ] Implement `AnthropicOAuthProvider` in `services/oauth-proxy/src/provider_impl.rs`
  - Holds `Arc<Pool>`
  - `id()` returns `"anthropic"`
  - `needs_body()` returns true
  - `prepare_request()`:
    - Call `pool.select()` to get account + access token
    - Strip client `Authorization` header from HeaderMap
    - Inject headers (exact values from spec):
      - `authorization: Bearer {access_token}`
      - `anthropic-beta: oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27` (merge/deduplicate with any client-provided `anthropic-beta` values)
      - `anthropic-dangerous-direct-browser-access: true`
      - `user-agent: claude-cli/2.0.76 (external, sdk-cli)`
      - `anthropic-version: 2023-06-01`
    - Call `inject_system_prompt(body, model)` on the mutable body
  - `classify_error()`: delegate to anthropic_pool quota classification (`classify_status`)
  - `report_error()`: delegate to pool.report_error()
  - `health()`: return pool health

- [ ] Implement `anthropic-beta` header merge/deduplication
  - Read client-provided `anthropic-beta` value if present
  - Split both client and required values by comma, deduplicate, rejoin
  - Tests: no client beta → required only, client with overlap → deduplicated, client with extra → merged

- [ ] Implement system prompt injection in `provider_impl.rs`
  - `extract_model(body)` — get `model` string from JSON body
  - `inject_system_prompt(body, model)` — for non-Haiku models, prepend `REQUIRED_SYSTEM_PROMPT_PREFIX`
  - Rules: no `system` field → create with prefix, existing `system` without prefix → prepend prefix + space, existing `system` with prefix → noop, haiku model → skip entirely
  - Tests: no system → inject, existing → prepend, existing with prefix → noop, haiku → skip

- [ ] Body handling fork in proxy.rs
  - If `provider.needs_body()`: deserialize body bytes into `serde_json::Value`, call `prepare_request(&mut headers, &mut body)`, serialize back to bytes for upstream
  - If `!provider.needs_body()`: pass `Value::Null` to prepare_request (provider ignores it), send original bytes
  - Handle deserialization failure: 400 Bad Request with `{"error": {"type": "invalid_request", "message": "Invalid JSON body"}}`

- [ ] Update proxy.rs error handling with failover loop
  - After upstream error response: call `provider.classify_error(status, body_str)`
  - QuotaExceeded → call `provider.report_error()`, retry with next account (re-call `prepare_request` with new headers)
  - Permanent → call `provider.report_error()`, return error to client
  - Transient → return error to client (existing retry logic handles timeouts)
  - Cap failover attempts at pool size to prevent infinite loops
  - Passthrough mode: no failover (classify always returns Transient, report is no-op)

- [ ] Update main.rs wiring for OAuth mode
  - Create CredentialStore, load credentials from file
  - Create Pool, populate from config.oauth.providers list + credential store
  - Spawn background refresh task
  - Create AnthropicOAuthProvider wrapping pool
  - Pass as `Arc<dyn Provider>` into ProxyState

- [ ] Upgrade health endpoint
  - Both modes: add `"mode"` field (`"passthrough"` or `"oauth_pool"`)
  - OAuth mode: add `"pool"` object with `status` (healthy/degraded/unhealthy), `accounts_total`, `accounts_available`, `accounts_cooling_down`, `accounts_disabled`, and `accounts` array
  - Each account in array: `{"id": "...", "status": "available"|"cooling_down"|"disabled"}` with `cooldown_remaining_secs` for cooling_down accounts
  - Status mapping: all available → healthy, some available → degraded, none available → unhealthy
  - Health endpoint always returns HTTP 200 (Kubernetes probes check JSON `status` field)
  - Passthrough mode: existing fields + `"mode": "passthrough"`, backward compatible

- [ ] Add new metrics with labels
  - `pool_account_status{account_id, status}` (gauge) — 1 for current status per account
  - `pool_failovers_total{from_account, reason}` (counter) — incremented on quota failover
  - `pool_token_refreshes_total{account_id, result}` (counter) — result: `success`/`failure`
  - `pool_quota_exhaustions_total{account_id}` (counter) — incremented when account enters CoolingDown

- [ ] Integration tests with mock Anthropic API
  - OAuth header injection (verify exact header values)
  - Authorization stripping (client auth header removed)
  - anthropic-beta merge/deduplication
  - System prompt injection (Opus, Sonnet, Haiku, no system, existing system, already-prefixed)
  - Quota failover (429 with quota message → next account)
  - Permanent disable (401 → account Disabled)
  - Pool exhausted 503 (correct JSON error shape)
  - Passthrough regression (all 86 existing tests still pass)

- [ ] Verify: `cargo test --workspace`, `cargo clippy` clean

---

## Phase 5: Admin API — not started

Goal: implement account management endpoints on a separate listener port.

- [ ] Implement admin router in `services/oauth-proxy/src/admin.rs`
  - Separate axum Router on config.admin.listen_addr (default `0.0.0.0:9090`)
  - Shared state: Arc<Pool>, Arc<CredentialStore>, Mutex<HashMap<String, PkceState>>
  - PkceState struct: `verifier: String`, `created_at: Instant`; expires after 10 minutes

- [ ] `GET /admin/accounts` — list accounts with status, never expose tokens
  - Response: `[{"id": "claude-max-...", "status": "available"|"cooling_down"|"disabled", ...}]`

- [ ] `POST /admin/accounts/init-oauth` — generate PKCE pair, return authorization URL + account_id
  - Account ID format: `claude-max-{unix_timestamp}` (e.g., `claude-max-1739059200`)
  - Response: `{"authorization_url": "...", "account_id": "claude-max-...", "instructions": "Open the URL in a browser, authorize, then paste the code to complete-oauth"}`
  - Store PkceState in-memory HashMap keyed by account_id

- [ ] `POST /admin/accounts/complete-oauth` — exchange code, store credential, add to pool
  - Request body: `{"account_id": "claude-max-...", "code": "{authorization_code}#{state}"}`
  - Parse `code#state`, exchange code with verifier from stored PkceState
  - Store credential in CredentialStore, add account to Pool
  - Return 400 if PkceState expired (>10 minutes) or not found

- [ ] `DELETE /admin/accounts/{id}` — remove from pool + credential file

- [ ] `GET /admin/pool` — pool summary (same shape as health endpoint pool object)

- [ ] Wire admin router in main.rs
  - Start only if config.admin.enabled and OAuth mode
  - Graceful shutdown alongside main listener

- [ ] PKCE state cleanup (lazy expiration: check age on access, remove expired entries)

- [ ] Tests: init-oauth URL validation, complete-oauth stores credential, expired PkceState returns 400, delete removes account, accounts list excludes tokens, admin isolation from proxy port

- [ ] Verify: `cargo test --workspace`, `cargo clippy` clean

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
