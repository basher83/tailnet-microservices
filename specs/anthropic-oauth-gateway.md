# Spec: Anthropic OAuth Gateway

**Status:** Draft
**Created:** 2026-02-09
**Author:** Brent + Cowork
**Supersedes:** `oauth-proxy.md` (header injector → full OAuth gateway)

---

## Overview

Evolve the anthropic-oauth-proxy from a static header injector into a full OAuth 2.0 gateway with subscription pooling. The proxy currently passes through client-provided `Authorization` headers and injects `anthropic-beta: oauth-2025-04-20`. The target state manages its own OAuth credentials: PKCE authentication flow, automatic token refresh, round-robin subscription pooling with quota failover, and the full Anthropic header contract.

Clients on the tailnet send unauthenticated requests. The gateway handles everything.

**Pins:**

| Reference | What it provides |
|-----------|-----------------|
| Loom `specs/claude-subscription-auth.md` | OAuth 2.0 PKCE flow, token exchange/refresh, credential storage, header contract |
| Loom `specs/anthropic-oauth-pool.md` | Subscription pooling, round-robin failover, quota detection, health reporting |
| Loom `specs/anthropic-max-pool-management.md` | Admin API patterns, proactive token refresh, account lifecycle |
| Aperture by Tailscale | Multi-provider gateway architecture, Tailscale identity, model-based routing |

**Design Principles (carried forward):**

| Principle | Implementation |
|-----------|----------------|
| Single responsibility | One binary, one provider (Anthropic) |
| Provider trait | Multi-provider interface designed in, only Anthropic implemented |
| Tailnet-native | MagicDNS identity via Tailscale Operator |
| Pure state machines | Auth state, pool state, service lifecycle — all explicit |
| Zero client credentials | Clients send bare requests; gateway injects everything |

---

## Architecture

```text
┌──────────────────────────────────────────────────────────────────────────┐
│                              Tailnet                                      │
│                                                                           │
│  ┌──────────┐     ┌──────────────┐     ┌──────────────────────────────┐  │
│  │ Aperture │────►│  Tailscale   │────►│  anthropic-oauth-proxy       │  │
│  │ (http://ai/)   │  Operator    │     │                              │  │
│  └──────────┘     │  Ingress     │     │  ┌────────────────────────┐  │  │
│                   └──────────────┘     │  │     OAuth Pool         │  │  │
│  ┌──────────┐                          │  │  ┌──────┐ ┌──────┐    │  │  │
│  │ForgeFlare│─────────────────────────►│  │  │ Max1 │ │ Max2 │... │  │  │
│  └──────────┘                          │  │  │  ✓   │ │  ⏳  │    │  │  │
│                                        │  │  └──────┘ └──────┘    │  │  │
│  ┌──────────┐                          │  └───────────┬────────────┘  │  │
│  │ Claude   │─────────────────────────►│              │               │  │
│  │ Code     │                          │              ▼               │  │
│  └──────────┘                          │  ┌────────────────────────┐  │  │
│                                        │  │  Header Injection      │  │  │
│                                        │  │  + System Prompt Gate  │  │  │
│                                        │  │  + Token Refresh       │  │  │
│                                        │  └───────────┬────────────┘  │  │
│                                        └──────────────┼───────────────┘  │
│                                                       │                  │
│                                                       ▼                  │
│                                              api.anthropic.com           │
└──────────────────────────────────────────────────────────────────────────┘
```

MagicDNS hostname remains `anthropic-oauth-proxy`. Same Tailscale Ingress, same K8s footprint. Binary name unchanged. The upgrade is internal.

---

## Auth Modes

The gateway supports two mutually exclusive auth modes, determined at startup:

| Mode | Source | Behavior |
|------|--------|----------|
| **Passthrough** (current) | `[headers]` config with no `[oauth]` section | Inject static headers, pass through client Authorization. Backward compatible. |
| **OAuth Pool** | `[oauth]` section present | Manage credentials, inject Bearer token + full header contract. Client Authorization ignored. |

This preserves backward compatibility. Existing deployments with `[[headers]]` config continue to work. Adding an `[oauth]` section activates the new behavior.

---

## Multi-Provider Interface

The gateway is Anthropic-only but the provider abstraction is designed for future extension.

### Provider Trait

```rust
/// Trait for LLM provider authentication and request preparation.
/// Anthropic is the only implementation. Designed for future providers
/// (OpenAI, Google) without breaking changes.
pub trait Provider: Send + Sync {
    /// Provider identifier (e.g., "anthropic")
    fn id(&self) -> &str;

    /// Prepare an outbound request: inject auth headers, modify body if needed.
    /// Called once per proxy attempt (not per retry).
    async fn prepare_request(
        &self,
        request: &mut RequestBuilder,
        body: &mut serde_json::Value,
    ) -> Result<(), ProviderError>;

    /// Classify an upstream error for failover decisions.
    fn classify_error(&self, status: u16, body: &str) -> ErrorClassification;

    /// Report an error back to the provider (e.g., mark account as cooling).
    async fn report_error(
        &self,
        classification: ErrorClassification,
    ) -> Result<(), ProviderError>;

    /// Health status for the /health endpoint.
    async fn health(&self) -> ProviderHealth;
}
```

### Error Classification

```rust
#[derive(Debug, Clone, Copy)]
pub enum ErrorClassification {
    /// Transient error, retry on same account (via existing retry loop)
    Transient,
    /// Quota exhausted, failover to next account in pool
    QuotaExceeded,
    /// Permanent error (bad credentials), disable account
    Permanent,
}
```

---

## OAuth Implementation

### Constants

```rust
/// Anthropic's public OAuth client ID (same as Claude CLI)
pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// OAuth redirect URI
pub const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";

/// Token endpoint
pub const TOKEN_ENDPOINT: &str = "https://console.anthropic.com/v1/oauth/token";

/// Authorization endpoint (Pro/Max subscriptions)
pub const AUTHORIZE_ENDPOINT: &str = "https://claude.ai/oauth/authorize";

/// OAuth scopes (user:sessions:claude_code required for Sonnet/Opus)
/// Note: org:create_api_key is NOT included — that's for Console OAuth (API key creation),
/// which is out of scope. This gateway only does Max (claude.ai) authorization.
pub const SCOPES: &str = "user:profile user:inference user:sessions:claude_code";

/// Required system prompt prefix for Opus/Sonnet access
pub const REQUIRED_SYSTEM_PROMPT_PREFIX: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";
```

### PKCE Flow

Standard OAuth 2.0 with PKCE (RFC 7636). No client secret (public client).

```text
Admin CLI                    Gateway                     claude.ai
    │                           │                            │
    │  add-account              │                            │
    ├──────────────────────────►│                            │
    │                           │  Generate PKCE pair        │
    │                           │  (verifier + S256 challenge)
    │  authorization_url        │                            │
    │◄──────────────────────────┤                            │
    │                           │                            │
    │  Open browser ───────────────────────────────────────►│
    │                           │                            │
    │  User authorizes          │                            │
    │◄──────────────────────────────────────────────────────┤
    │                           │                            │
    │  Paste code#{state}       │                            │
    ├──────────────────────────►│                            │
    │                           │  POST /v1/oauth/token      │
    │                           ├───────────────────────────►│
    │                           │  {access, refresh, expires}│
    │                           │◄───────────────────────────┤
    │                           │                            │
    │                           │  Store credentials         │
    │                           │  Add to pool (hot-reload)  │
    │  Account added            │                            │
    │◄──────────────────────────┤                            │
```

### Credential Storage

JSON file on disk, keyed by account ID. Mounted as a PersistentVolume in K8s.

```json
{
  "claude-max-1": {
    "type": "oauth",
    "refresh": "rt_abc123...",
    "access": "at_xyz789...",
    "expires": 1735500000000
  },
  "claude-max-2": {
    "type": "oauth",
    "refresh": "rt_def456...",
    "access": "at_uvw000...",
    "expires": 1735500100000
  }
}
```

File permissions: `0600`, owned by container UID 1000 (non-root). Never exposed via API. Health endpoint shows account IDs and status only.

**Atomic writes:** Credential file updates use write-to-temp + atomic rename to prevent corruption on crash mid-write. All writes acquire an in-memory `Mutex` to prevent concurrent modification from request-time refresh and background refresh tasks.

**Cold start:** If credential file does not exist, create it as `{}`. Pool starts with zero accounts, health reports `unhealthy` with `accounts_total: 0`. First request returns 503 until an account is added via admin API.

---

## Subscription Pool

### Account State Machine

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    /// Ready to serve requests
    Available,
    /// Hit 5-hour quota, cooling down
    CoolingDown { until: Instant },
    /// Permanently failed (invalid credentials, revoked token)
    Disabled,
}
```

### State Transitions

| Current | Trigger | New State | Action |
|---------|---------|-----------|--------|
| `Available` | Request succeeds | `Available` | None |
| `Available` | 429 + quota message | `CoolingDown` | Log, failover to next |
| `Available` | 401/403 | `Disabled` | Log error, failover |
| `Available` | Transient error | `Available` | Retry (existing retry loop) |
| `CoolingDown` | Cooldown expired | `Available` | Log recovery |
| `CoolingDown` | Token refresh fails | `Disabled` | Log error |
| `Disabled` | Admin removes | (removed) | Persist credential file |

State transitions apply uniformly: a 401/403 from token refresh (request-time or background) transitions the account to `Disabled` regardless of previous state. `CoolingDown` accounts that fail background refresh go directly to `Disabled`.

### Account Selection (Round-Robin)

1. Start from `next_index`
2. Scan N accounts for `Available` status
3. Check `CoolingDown` accounts — if cooldown expired, transition to `Available`
4. Select first `Available`, advance `next_index`
5. If none available → 503 Service Unavailable:
```json
{
  "error": {
    "type": "pool_exhausted",
    "message": "All accounts exhausted",
    "pool": {
      "accounts_total": 2,
      "accounts_available": 0,
      "accounts_cooling": 2,
      "accounts_disabled": 0
    }
  }
}
```

### Quota Detection

Not all 429s are quota exhaustion. The classifier inspects the error message body:

| HTTP Status | Message Pattern | Classification |
|-------------|----------------|----------------|
| 429 | Contains "5-hour", "5 hour", "rolling window", "usage limit for your plan", "subscription usage limit" | `QuotaExceeded` |
| 429 | Other rate limit messages | `Transient` |
| 401, 403 | Any | `Permanent` |
| 408, 500, 502, 503, 504 | Any | `Transient` |

### Cooldown

Default: 2 hours (configurable via `cooldown_secs`). Conservative — the actual 5-hour window is rolling, so 2h is a safe recovery margin.

---

## Request Pipeline

The gateway modifies both headers and body before forwarding.

### Header Injection (OAuth Mode)

| Header | Value | Notes |
|--------|-------|-------|
| `Authorization` | `Bearer {access_token}` | From selected pool account. Client-provided Authorization is **ignored** (stripped before injection). |
| `anthropic-beta` | `oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27` | Must include `oauth-2025-04-20` |
| `anthropic-dangerous-direct-browser-access` | `true` | Required for OAuth |
| `user-agent` | `claude-cli/2.0.76 (external, sdk-cli)` | Must match Claude CLI format |
| `anthropic-version` | `2023-06-01` | API version |

Client-provided `anthropic-beta` values are merged (deduplicated) with the required set.

### System Prompt Prefix Injection

For Opus and Sonnet models, the system prompt **must** start with the exact phrase:

```text
You are Claude Code, Anthropic's official CLI for Claude.
```

The gateway inspects the request body:

| Condition | Action |
|-----------|--------|
| No `system` field | Inject `system` with required prefix |
| `system` field exists, missing prefix | Prepend prefix + `" "` + existing content |
| `system` field exists, has prefix | No modification |
| Model is Haiku | No modification (prefix not required) |

```rust
fn inject_system_prompt(body: &mut serde_json::Value, model: &str) {
    // Haiku doesn't need the prefix
    if model.contains("haiku") {
        return;
    }

    let prefix = REQUIRED_SYSTEM_PROMPT_PREFIX;

    match body.get_mut("system") {
        None => {
            body["system"] = serde_json::Value::String(prefix.to_string());
        }
        Some(system) => {
            if let Some(s) = system.as_str() {
                if !s.starts_with(prefix) {
                    *system = serde_json::Value::String(
                        format!("{} {}", prefix, s)
                    );
                }
            }
        }
    }
}
```

### Model Extraction

The gateway extracts the model name from the request body to determine:

1. Whether system prompt injection is needed (non-Haiku)
2. Logging and metrics labels

```rust
fn extract_model(body: &serde_json::Value) -> Option<&str> {
    body.get("model").and_then(|v| v.as_str())
}
```

### Body Handling Path

The current proxy forwards request bodies as opaque bytes. OAuth mode requires body modification (system prompt injection), which means `bytes → serde_json::Value → modify → serialize → forward`. This is the riskiest refactor — it touches the hot path and introduces a JSON round-trip that could alter field ordering, whitespace, or unicode escapes.

| Mode | Body Path | Cost |
|------|-----------|------|
| Passthrough | Opaque bytes forwarded unchanged | Zero overhead (existing behavior) |
| OAuth | Deserialize → modify → serialize | JSON round-trip on every request |

Design choice: **always deserialize in OAuth mode.** The latency of an Anthropic API call (seconds) dwarfs JSON serde time (microseconds). Optimizing with string-scan peeking adds complexity for negligible gain at this scale.

The `Provider` trait receives `&mut serde_json::Value` so providers that don't need body modification (future OpenAI provider) can simply no-op. The deserialization happens once in `proxy.rs` before calling `provider.prepare_request()`, only when the provider is in OAuth mode.

---

## Token Refresh

### Auto-Refresh on Request

Before each proxied request, the gateway checks the selected account's token expiration:

| Condition | Action |
|-----------|--------|
| Token valid (>60s remaining) | Use current access token |
| Token expiring (<60s) or expired | Refresh via `POST /v1/oauth/token` with `grant_type=refresh_token` |
| Refresh succeeds | Update in-memory + persist to credential file |
| Refresh fails | Mark account `Disabled`, failover to next |

### Proactive Background Refresh

A background tokio task runs independently of request flow:

| Parameter | Default | Config Key |
|-----------|---------|------------|
| Check interval | 5 minutes | `refresh_interval_secs` |
| Refresh threshold | 15 minutes | `refresh_threshold_secs` |

The task iterates all accounts, refreshing any token expiring within the threshold. This prevents mid-request refresh latency under normal operation.

---

## Configuration

### File Format (TOML)

```toml
# anthropic-oauth-proxy.toml

[proxy]
listen_addr = "0.0.0.0:8080"
upstream_url = "https://api.anthropic.com"
timeout_secs = 60
max_connections = 1000

# OAuth pool configuration (presence activates OAuth mode)
[oauth]
credential_file = "/data/credentials.json"
cooldown_secs = 7200          # 2 hours
refresh_interval_secs = 300   # 5 minutes
refresh_threshold_secs = 900  # 15 minutes

# Static accounts to load from credential file at startup
# New accounts can be added at runtime via admin API
providers = ["claude-max-1", "claude-max-2"]

# Admin API (for account management)
[admin]
enabled = true
listen_addr = "0.0.0.0:9090"  # Separate port, not exposed via Ingress
```

When `[oauth]` is absent, the gateway falls back to passthrough mode using `[[headers]]` (backward compatible with current config). If both `[oauth]` and `[[headers]]` are present, `[oauth]` takes precedence and `[[headers]]` is ignored.

### Environment Variables

| Variable | Description |
|----------|-------------|
| `CONFIG_PATH` | Config file path (existing) |
| `LOG_LEVEL` | Logging verbosity (existing) |
| `CREDENTIAL_FILE` | Override credential file path |

---

## Admin API

Separate listener on a non-Ingress port. Accessed via `kubectl port-forward` (authenticated by Kubernetes kubeconfig). Not exposed to the tailnet.

**Single-pod requirement:** PKCE state is in-memory. The gateway runs as a single-replica Deployment. Multi-pod is not supported for the admin API flow (init-oauth on pod A, complete-oauth on pod B would fail). This is acceptable — account management is a rare admin operation, not a hot path.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/accounts` | List accounts with status |
| `POST` | `/admin/accounts/init-oauth` | Start OAuth PKCE flow, returns authorization URL |
| `POST` | `/admin/accounts/complete-oauth` | Complete OAuth flow with authorization code |
| `DELETE` | `/admin/accounts/{id}` | Remove account from pool |
| `GET` | `/admin/pool` | Pool status summary |

### Account ID Generation

New accounts are assigned IDs in the format `claude-max-{unix_timestamp}` (e.g., `claude-max-1739059200`). Must be unique within the credential file.

### PKCE State Storage

Between `init-oauth` and `complete-oauth`, the gateway stores PKCE verifiers in an in-memory `HashMap<String, PkceState>` keyed by account ID. Entries expire after 10 minutes. No persistence needed — if the gateway restarts mid-flow, the admin simply re-initiates.

```rust
struct PkceState {
    verifier: String,
    created_at: Instant,
}
```

### Init OAuth Response

```json
{
  "authorization_url": "https://claude.ai/oauth/authorize?client_id=...&code_challenge=...&state=...",
  "account_id": "claude-max-1739059200",
  "instructions": "Open the URL in a browser, authorize, then paste the code to complete-oauth"
}
```

### Complete OAuth Request

```json
{
  "account_id": "claude-max-1739059200",
  "code": "{authorization_code}#{state}"
}
```

---

## Health Endpoint (Upgraded)

The existing `/health` endpoint expands to include pool status:

### Passthrough Mode (backward compatible)

```json
{
  "status": "healthy",
  "mode": "passthrough",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0
}
```

### OAuth Pool Mode

```json
{
  "status": "healthy",
  "mode": "oauth_pool",
  "uptime_seconds": 3600,
  "requests_served": 12345,
  "errors_total": 0,
  "pool": {
    "accounts_total": 3,
    "accounts_available": 2,
    "accounts_cooling": 1,
    "accounts_disabled": 0,
    "accounts": [
      { "id": "claude-max-1", "status": "available" },
      { "id": "claude-max-2", "status": "cooling_down", "cooldown_remaining_secs": 3600 },
      { "id": "claude-max-3", "status": "available" }
    ]
  }
}
```

### Health Status Mapping

| Pool State | Status |
|------------|--------|
| All available | `healthy` |
| ≥1 available, some cooling/disabled | `degraded` |
| All cooling or disabled | `unhealthy` (still returns 200 — Kubernetes probes check JSON) |

---

## Metrics (Extended)

Existing metrics are preserved. New metrics for OAuth:

| Metric | Type | Labels |
|--------|------|--------|
| `proxy_requests_total` | Counter | `status`, `method` (existing) |
| `proxy_request_duration_seconds` | Histogram | `status` (existing) |
| `proxy_upstream_errors_total` | Counter | `error_type` (existing) |
| `pool_account_status` | Gauge | `account_id`, `status` |
| `pool_failovers_total` | Counter | `from_account`, `reason` |
| `pool_token_refreshes_total` | Counter | `account_id`, `result` |
| `pool_quota_exhaustions_total` | Counter | `account_id` |

---

## Project Structure (Target)

```text
crates/
  common/               # Shared types: error types (existing)
  provider/             # NEW: Provider trait, ErrorClassification
  anthropic-auth/       # NEW: OAuth PKCE, token refresh, credential storage
  anthropic-pool/       # NEW: Subscription pool, round-robin, failover
services/
  oauth-proxy/          # Existing binary, upgraded
    src/
      main.rs           # Entry point (existing, extended)
      config.rs         # Config (existing, extended with [oauth] section)
      service.rs        # Service state machine (existing)
      proxy.rs          # HTTP proxy (existing, call provider.prepare_request())
      metrics.rs        # Metrics (existing, extended)
      admin.rs          # NEW: Admin API routes
      provider_impl.rs  # NEW: AnthropicProvider implementing Provider trait
specs/
  oauth-proxy.md              # Original spec (Complete, preserved)
  anthropic-oauth-gateway.md  # This spec
```

### Crate Dependency Graph

```text
oauth-proxy (binary)
  ├── common
  ├── provider          (trait definition)
  ├── anthropic-auth    (PKCE, token refresh, credentials)
  └── anthropic-pool    (pool state, selection, failover)
        └── anthropic-auth
```

---

## Deployment Changes

### PersistentVolumeClaim

Credential file must survive pod restarts. Add a PVC mounted at `/data/`:

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: anthropic-oauth-credentials
  namespace: tailnet
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 1Mi
```

### ConfigMap Update

Add `[oauth]` section to the existing ConfigMap. Backward compatible — removing the section reverts to passthrough mode.

### Service (Admin)

Optional second Service on port 9090 for admin API. Not exposed via Ingress. Accessible via `kubectl port-forward` or from within the cluster.

### No Other Changes

Same Deployment, same Ingress, same MagicDNS hostname, same container image tag pattern. The binary detects its mode from config.

---

## Implementation Phases

### Phase 1: Provider Abstraction + Mode Detection

- [ ] Create `crates/provider/` with `Provider` trait and `ErrorClassification`
- [ ] Create `PassthroughProvider` implementing `Provider` trait (wraps current header injection logic)
- [ ] Extend `config.rs` with `[oauth]` section parsing and mode detection
- [ ] Refactor `proxy.rs` to call `provider.prepare_request()` instead of inline header injection
- [ ] Body handling fork: passthrough = opaque bytes, oauth = deserialize (stubbed — passthrough provider uses opaque path)
- [ ] All existing tests pass, behavior unchanged (passthrough provider reproduces current behavior exactly)

### Phase 2: OAuth Foundation

- [ ] Create `crates/anthropic-auth/` with PKCE generation (verifier + S256 challenge)
- [ ] Implement token exchange (`authorization_code` grant)
- [ ] Implement token refresh (`refresh_token` grant)
- [ ] Implement credential file storage (read/write JSON, 0600 permissions)
- [ ] Unit tests for PKCE, exchange, refresh, storage

### Phase 3: Subscription Pool

- [ ] Create `crates/anthropic-pool/` with `AccountStatus` state machine
- [ ] Implement round-robin account selection
- [ ] Implement quota detection (429 message parsing)
- [ ] Implement cooldown management
- [ ] Implement proactive background refresh task
- [ ] Unit tests for selection, failover, cooldown transitions, quota detection

### Phase 4: Gateway Integration

- [ ] Implement `AnthropicOAuthProvider` using pool + auth crates
- [ ] Header injection: Bearer token, beta flags, User-Agent, dangerous-direct-browser-access
- [ ] System prompt prefix injection (body modification for Opus/Sonnet)
- [ ] Model extraction from request body
- [ ] Extend config.rs with `[oauth]` section parsing
- [ ] Mode detection: passthrough vs oauth_pool
- [ ] Extend health endpoint with pool status
- [ ] Add pool metrics
- [ ] Integration tests with mock Anthropic API

### Phase 5: Admin API

- [ ] Implement admin router on separate port
- [ ] `GET /admin/accounts` — list with pool status
- [ ] `POST /admin/accounts/init-oauth` — generate PKCE, return authorization URL
- [ ] `POST /admin/accounts/complete-oauth` — exchange code, store credentials, add to pool
- [ ] `DELETE /admin/accounts/{id}` — remove from pool, update credential file
- [ ] `GET /admin/pool` — pool summary

### Phase 6: Deployment

- [ ] Add PVC manifest for credential storage
- [ ] Update ConfigMap with `[oauth]` section
- [ ] Optional admin Service manifest
- [ ] Update RUNBOOK.md with OAuth operational procedures
- [ ] E2E test: add account → proxy request → verify Claude API response

---

## Success Criteria

- [ ] Backward compatible: existing `[[headers]]` config works unchanged
- [ ] OAuth PKCE flow completes: admin adds account via CLI/API
- [ ] Token auto-refresh: no manual token management after initial auth
- [ ] Pool failover: quota exhaustion on account A → automatic switch to account B
- [ ] System prompt injection: Opus/Sonnet requests get required prefix
- [ ] Health endpoint: reports pool status (available/cooling/disabled per account)
- [ ] Credential persistence: pod restart preserves OAuth tokens
- [ ] Zero client credentials: ForgeFlare/Claude Code send bare requests

---

## Security Considerations

1. **Credential isolation**: Each account's OAuth tokens independently managed
2. **No credential exposure**: Admin API and health endpoint show account IDs and status, never tokens
3. **File permissions**: Credential file 0600, owned by container user
4. **Admin API isolation**: Separate port, not exposed via Tailscale Ingress
5. **System prompt prefix**: Required by Anthropic for Opus/Sonnet — gateway handles this transparently
6. **PKCE**: All OAuth flows use S256 challenge to prevent code interception

---

## Out of Scope

- **Non-Anthropic providers** — multi-provider trait is designed in, but only Anthropic is implemented
- **Web UI for account management** — admin API is CLI/curl-driven. Web UI is a future enhancement.
- **Telemetry/session tracking** — Aperture handles this upstream. The gateway is auth-only.
- **Rate limiting / access control** — Aperture handles per-user access. The gateway serves all tailnet traffic equally.
- **Console OAuth mode** — only Max (claude.ai) authorization is supported. API key creation via OAuth is not needed.

---

## Header Discovery Maintenance

When Anthropic updates Claude CLI, required headers may change. Maintenance procedure:

1. Install updated Claude CLI
2. Sniff traffic via mitmproxy: `mitmdump --set flow_detail=4 -p 8888`
3. Route CLI through proxy: `HTTPS_PROXY=http://127.0.0.1:8888 claude --print "hello"`
4. Compare captured headers against constants in `anthropic-auth`
5. Update constants, run tests

---

## References

- Loom `specs/claude-subscription-auth.md` — OAuth 2.0 PKCE implementation details
- Loom `specs/anthropic-oauth-pool.md` — Subscription pooling architecture
- Loom `specs/anthropic-max-pool-management.md` — Admin API patterns
- `aperture/aperture.md` — Multi-provider gateway reference architecture
- `specs/oauth-proxy.md` — Current implementation (passthrough header injector)
- [OAuth 2.0 PKCE RFC 7636](https://datatracker.ietf.org/doc/html/rfc7636)
- [Anthropic MAX Plan Implementation Guide](https://raw.githubusercontent.com/nsxdavid/anthropic-max-router/main/ANTHROPIC-MAX-PLAN-IMPLEMENTATION-GUIDE.md)

---

*Spec-first. Types define contract. State machine is the program.*
