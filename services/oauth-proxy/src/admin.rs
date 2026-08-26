//! Admin API for account management
//!
//! Runs on a separate listener port (default 9090) and provides endpoints for
//! managing OAuth accounts in the pool. Not exposed via Tailscale Ingress —
//! accessed via `kubectl port-forward`.
//!
//! Endpoints:
//! - GET  /admin/accounts         — list accounts with status
//! - POST /admin/accounts/init-oauth    — start PKCE flow, return auth URL
//! - POST /admin/accounts/complete-oauth — exchange code, store credential, add to pool
//! - DELETE /admin/accounts/:id   — remove account from pool + credential store
//! - GET  /admin/pool             — pool status summary

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, warn};

use anthropic_pool::Pool;

/// In-memory PKCE state for an in-progress OAuth flow.
///
/// Created by init-oauth and consumed by complete-oauth. Keyed by the OAuth
/// `state` value (a random 43-char base64url string — Anthropic rejects an
/// id-shaped `state`, so the account id is carried here instead of being used
/// as the key). Expires after PKCE_EXPIRY_SECS to prevent stale verifiers
/// from accumulating.
struct PkceState {
    account_id: String,
    verifier: String,
    created_at: Instant,
}

/// Maximum age of a PKCE state entry before it expires.
const PKCE_EXPIRY_SECS: u64 = 600; // 10 minutes

/// Shared state for admin API handlers.
#[derive(Clone)]
pub struct AdminState {
    pool: Arc<Pool>,
    http_client: reqwest::Client,
    pkce_states: Arc<Mutex<HashMap<String, PkceState>>>,
}

impl AdminState {
    pub fn new(pool: Arc<Pool>, http_client: reqwest::Client) -> Self {
        Self {
            pool,
            http_client,
            pkce_states: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Build the admin axum router with all account management endpoints.
pub fn build_admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/accounts", get(list_accounts))
        .route("/admin/accounts/init-oauth", post(init_oauth))
        .route("/admin/accounts/complete-oauth", post(complete_oauth))
        .route("/admin/accounts/{id}", delete(delete_account))
        .route("/admin/pool", get(pool_status))
        .with_state(state)
}

/// GET /admin/accounts — list all accounts with their pool status.
///
/// Never exposes tokens. Returns account IDs and their current status
/// (available, cooling_down, disabled).
async fn list_accounts(State(state): State<AdminState>) -> impl IntoResponse {
    let health = state.pool.health().await;
    let accounts = health
        .get("accounts")
        .cloned()
        .unwrap_or(serde_json::json!([]));

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "accounts": accounts }).to_string(),
    )
}

/// POST /admin/accounts/init-oauth — generate PKCE pair and return authorization URL.
///
/// Creates a new account ID from the current unix timestamp, generates a PKCE
/// verifier + challenge and a random OAuth `state`, builds the authorization
/// URL, and stores the verifier in memory (keyed by `state`) for
/// complete-oauth to consume. The response carries both `account_id` and
/// `state`; the browser callback shows `code#state`, which is all
/// complete-oauth needs.
async fn init_oauth(State(state): State<AdminState>) -> impl IntoResponse {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let account_id = format!("claude-max-{timestamp}");

    let verifier = anthropic_auth::generate_verifier();
    let challenge = anthropic_auth::compute_challenge(&verifier);
    let oauth_state = anthropic_auth::generate_state();
    let authorization_url = anthropic_auth::build_authorization_url(&oauth_state, &challenge);

    // Store PKCE state for complete-oauth to consume, keyed by `state`
    let pkce_state = PkceState {
        account_id: account_id.clone(),
        verifier,
        created_at: Instant::now(),
    };

    let mut states = state.pkce_states.lock().await;
    // Lazy cleanup: remove expired entries while holding the lock
    states.retain(|_, s| s.created_at.elapsed().as_secs() < PKCE_EXPIRY_SECS);
    states.insert(oauth_state.clone(), pkce_state);

    info!(account_id, "PKCE flow initiated");

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "authorization_url": authorization_url,
            "account_id": account_id,
            "state": oauth_state,
            "instructions": "Open the URL in a browser, authorize, then paste the code#state value to complete-oauth"
        })
        .to_string(),
    )
}

/// Request body for complete-oauth endpoint.
///
/// `code` is the `code#state` value shown by the browser callback. `state`
/// may be given separately if the pasted code has no `#state` suffix.
/// `account_id` is optional; when present it must match the flow that
/// produced `state`.
#[derive(Deserialize)]
struct CompleteOAuthRequest {
    code: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

fn json_error(
    status: StatusCode,
    msg: impl Into<String>,
) -> (
    StatusCode,
    [(axum::http::HeaderName, &'static str); 1],
    String,
) {
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "error": msg.into() }).to_string(),
    )
}

/// POST /admin/accounts/complete-oauth — exchange authorization code for tokens.
///
/// Parses `code#state` from the callback, retrieves the PKCE verifier from the
/// in-memory store by `state`, exchanges the code via the token endpoint,
/// stores the credential, and adds the account to the pool.
async fn complete_oauth(
    State(state): State<AdminState>,
    axum::Json(body): axum::Json<CompleteOAuthRequest>,
) -> impl IntoResponse {
    // Parse code#state — the callback shows both joined by '#'
    let (authorization_code, state_from_code) = match body.code.split_once('#') {
        Some((c, s)) => (c.to_string(), Some(s.to_string())),
        None => (body.code.clone(), None),
    };
    let oauth_state = match state_from_code.or_else(|| body.state.clone()) {
        Some(s) if !s.is_empty() => s,
        _ => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "missing state: pass the full code#state value from the callback, or a separate \"state\" field",
            );
        }
    };

    // Retrieve and remove PKCE state
    let pkce_state = {
        let mut states = state.pkce_states.lock().await;
        states.remove(&oauth_state)
    };

    let pkce_state = match pkce_state {
        Some(s) => s,
        None => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "no pending OAuth flow for this state (expired or not initiated)",
            );
        }
    };

    let account_id = pkce_state.account_id.clone();
    if let Some(given) = &body.account_id
        && given != &account_id
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            format!("account_id {given} does not match the flow for this state ({account_id})"),
        );
    }

    // Check expiration
    if pkce_state.created_at.elapsed() > Duration::from_secs(PKCE_EXPIRY_SECS) {
        return (
            StatusCode::BAD_REQUEST,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::json!({
                "error": "PKCE state expired (>10 minutes), please re-initiate with init-oauth"
            })
            .to_string(),
        );
    }

    // Exchange code for tokens
    let token_response = match anthropic_auth::exchange_code(
        &state.http_client,
        &authorization_code,
        &oauth_state,
        &pkce_state.verifier,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(account_id = account_id, error = %e, "token exchange failed");
            return (
                StatusCode::BAD_GATEWAY,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::json!({
                    "error": format!("token exchange failed: {e}")
                })
                .to_string(),
            );
        }
    };

    // Compute absolute expiration timestamp
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let expires = now_millis + (token_response.expires_in * 1000);

    let credential = anthropic_auth::Credential {
        credential_type: "oauth".to_string(),
        refresh: token_response.refresh_token,
        access: token_response.access_token,
        expires,
    };

    // Store credential and add to pool
    let credential_store = state.pool.credential_store();
    if let Err(e) = credential_store.add(account_id.clone(), credential).await {
        warn!(account_id = account_id, error = %e, "failed to store credential");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::json!({
                "error": format!("failed to store credential: {e}")
            })
            .to_string(),
        );
    }

    state.pool.add_account(account_id.clone()).await;

    info!(
        account_id = account_id,
        "OAuth flow completed, account added to pool"
    );

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "account_id": account_id,
            "status": "added"
        })
        .to_string(),
    )
}

/// DELETE /admin/accounts/:id — remove account from pool and credential store.
async fn delete_account(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    state.pool.remove_account(&id).await;

    let credential_store = state.pool.credential_store();
    if let Err(e) = credential_store.remove(&id).await {
        warn!(account_id = id, error = %e, "credential removal failed (account already removed from pool)");
    }

    info!(account_id = id, "account removed");

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "account_id": id,
            "status": "removed"
        })
        .to_string(),
    )
}

/// GET /admin/pool — pool status summary (same shape as health endpoint pool object).
async fn pool_status(State(state): State<AdminState>) -> impl IntoResponse {
    let health = state.pool.health().await;

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        health.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// Create a test pool with a temporary credential store.
    async fn test_pool(dir: &std::path::Path) -> Arc<Pool> {
        let cred_path = dir.join("credentials.json");
        let store = anthropic_auth::CredentialStore::load(cred_path)
            .await
            .unwrap();
        let store = Arc::new(store);
        Arc::new(Pool::new(
            vec![],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        ))
    }

    fn test_admin_state(pool: Arc<Pool>) -> AdminState {
        AdminState::new(pool, reqwest::Client::new())
    }

    #[tokio::test]
    async fn list_accounts_empty_pool() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["accounts"], serde_json::json!([]));
    }

    async fn post_json(
        app: Router,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn init_oauth_uses_random_state_and_code_true() {
        // Regression for the 2026-08-26 finding: Anthropic's authorize page
        // fails on load without `code=true`, and rejects the Authorize POST
        // with "Invalid request format" when `state` is the id-shaped
        // `claude-max-<ts>`. The flow must key on a 43-char random state.
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);
        let app = build_admin_router(state.clone());

        let (status, json) =
            post_json(app, "/admin/accounts/init-oauth", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK);

        let url = json["authorization_url"].as_str().unwrap();
        let oauth_state = json["state"].as_str().unwrap();
        let account_id = json["account_id"].as_str().unwrap();

        assert!(url.contains("?code=true&"), "missing code=true: {url}");
        assert!(
            url.ends_with(&format!("&state={oauth_state}")),
            "state not in url: {url}"
        );
        assert_eq!(
            oauth_state.len(),
            43,
            "state must be 32 random bytes base64url"
        );
        assert!(
            !url.contains(&format!("state={account_id}")),
            "state must not be the account id"
        );
        assert!(account_id.starts_with("claude-max-"));

        // Map is keyed by state and carries the account id
        let states = state.pkce_states.lock().await;
        let entry = states.get(oauth_state).expect("pkce entry keyed by state");
        assert_eq!(entry.account_id, account_id);
        assert!(!states.contains_key(account_id));
    }

    #[tokio::test]
    async fn complete_oauth_rejects_missing_or_unknown_state() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);

        // No '#state' suffix and no separate state field → 400, never a network call
        let (status, json) = post_json(
            build_admin_router(state.clone()),
            "/admin/accounts/complete-oauth",
            serde_json::json!({ "code": "abc" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("missing state"));

        // Unknown state → 400
        let (status, json) = post_json(
            build_admin_router(state.clone()),
            "/admin/accounts/complete-oauth",
            serde_json::json!({ "code": "abc#nope" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .contains("no pending OAuth flow")
        );
    }

    #[tokio::test]
    async fn complete_oauth_rejects_account_id_mismatch_and_consumes_state() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);

        let (_, init) = post_json(
            build_admin_router(state.clone()),
            "/admin/accounts/init-oauth",
            serde_json::json!({}),
        )
        .await;
        let oauth_state = init["state"].as_str().unwrap().to_string();

        // Wrong account_id for this state → 400, and the entry is consumed
        // (single-use), so a retry reports no pending flow.
        let (status, json) = post_json(
            build_admin_router(state.clone()),
            "/admin/accounts/complete-oauth",
            serde_json::json!({ "code": format!("abc#{oauth_state}"), "account_id": "claude-max-other" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("does not match"));
        assert!(!state.pkce_states.lock().await.contains_key(&oauth_state));
    }

    #[tokio::test]
    async fn list_accounts_with_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;

        // Add a credential and account
        let credential = anthropic_auth::Credential {
            credential_type: "oauth".to_string(),
            refresh: "rt_test".to_string(),
            access: "at_test".to_string(),
            expires: u64::MAX,
        };
        pool.credential_store()
            .add("test-account".to_string(), credential)
            .await
            .unwrap();
        pool.add_account("test-account".to_string()).await;

        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let accounts = json["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["id"], "test-account");
        assert_eq!(accounts[0]["status"], "available");
        // Verify tokens are never exposed
        assert!(accounts[0].get("access").is_none());
        assert!(accounts[0].get("refresh").is_none());
    }

    #[tokio::test]
    async fn init_oauth_returns_authorization_url() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/accounts/init-oauth")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Verify response shape
        assert!(
            json["authorization_url"]
                .as_str()
                .unwrap()
                .starts_with("https://claude.ai/oauth/authorize")
        );
        assert!(
            json["account_id"]
                .as_str()
                .unwrap()
                .starts_with("claude-max-")
        );
        assert!(json["instructions"].as_str().is_some());
    }

    #[tokio::test]
    async fn complete_oauth_without_init_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/accounts/complete-oauth")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "account_id": "claude-max-999",
                            "code": "fake-code#fake-state"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .contains("no pending OAuth flow")
        );
    }

    #[tokio::test]
    async fn expired_pkce_state_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = AdminState::new(pool, reqwest::Client::new());

        // Manually insert an expired PKCE state
        {
            let mut states = state.pkce_states.lock().await;
            states.insert(
                "test-state".to_string(),
                PkceState {
                    account_id: "claude-max-expired".to_string(),
                    verifier: "test-verifier".to_string(),
                    // Set created_at far in the past
                    created_at: Instant::now() - Duration::from_secs(PKCE_EXPIRY_SECS + 60),
                },
            );
        }

        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/accounts/complete-oauth")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "account_id": "claude-max-expired",
                            "code": "test-code#test-state"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("expired"));
    }

    #[tokio::test]
    async fn delete_account_removes_from_pool() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;

        // Add account first
        let credential = anthropic_auth::Credential {
            credential_type: "oauth".to_string(),
            refresh: "rt_test".to_string(),
            access: "at_test".to_string(),
            expires: u64::MAX,
        };
        pool.credential_store()
            .add("delete-me".to_string(), credential)
            .await
            .unwrap();
        pool.add_account("delete-me".to_string()).await;

        // Verify account exists
        assert_eq!(pool.account_ids().await.len(), 1);

        let state = test_admin_state(pool.clone());
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/accounts/delete-me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["account_id"], "delete-me");
        assert_eq!(json["status"], "removed");

        // Verify account is actually removed
        assert_eq!(pool.account_ids().await.len(), 0);
        assert!(pool.credential_store().get("delete-me").await.is_none());
    }

    #[tokio::test]
    async fn pool_status_returns_pool_health() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/pool")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Empty pool should report unhealthy
        assert_eq!(json["status"], "unhealthy");
        assert_eq!(json["accounts_total"], 0);
        assert_eq!(json["accounts_available"], 0);
    }

    #[tokio::test]
    async fn pool_status_with_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;

        let credential = anthropic_auth::Credential {
            credential_type: "oauth".to_string(),
            refresh: "rt_test".to_string(),
            access: "at_test".to_string(),
            expires: u64::MAX,
        };
        pool.credential_store()
            .add("pool-acct".to_string(), credential)
            .await
            .unwrap();
        pool.add_account("pool-acct".to_string()).await;

        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/pool")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "healthy");
        assert_eq!(json["accounts_total"], 1);
        assert_eq!(json["accounts_available"], 1);
    }

    #[tokio::test]
    async fn init_oauth_stores_pkce_state() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = AdminState::new(pool, reqwest::Client::new());
        let pkce_states = state.pkce_states.clone();
        let app = build_admin_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/accounts/init-oauth")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let account_id = json["account_id"].as_str().unwrap();
        let oauth_state = json["state"].as_str().unwrap();

        // Verify PKCE state was stored, keyed by the OAuth state (not the
        // account id — Anthropic rejects an id-shaped `state`).
        let states = pkce_states.lock().await;
        assert!(states.contains_key(oauth_state));
        assert!(!states.contains_key(account_id));
        assert_eq!(states[oauth_state].account_id, account_id);
    }

    #[tokio::test]
    async fn admin_routes_isolated_from_proxy_port() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        // Admin router should not handle proxy-style paths
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Non-admin routes should 404 on the admin router
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_nonexistent_account_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path()).await;
        let state = test_admin_state(pool);
        let app = build_admin_router(state);

        // Deleting a nonexistent account should succeed (idempotent)
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/accounts/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
