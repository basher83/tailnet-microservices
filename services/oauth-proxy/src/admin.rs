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
/// Created by init-oauth and consumed by complete-oauth. Expires after
/// PKCE_EXPIRY_SECS to prevent stale verifiers from accumulating.
struct PkceState {
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
/// verifier + challenge, builds the authorization URL, and stores the verifier
/// in memory for complete-oauth to consume.
async fn init_oauth(State(state): State<AdminState>) -> impl IntoResponse {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let account_id = format!("claude-max-{timestamp}");

    let verifier = anthropic_auth::generate_verifier();
    let challenge = anthropic_auth::compute_challenge(&verifier);
    let authorization_url = anthropic_auth::build_authorization_url(&account_id, &challenge);

    // Store PKCE state for complete-oauth to consume
    let pkce_state = PkceState {
        verifier,
        created_at: Instant::now(),
    };

    let mut states = state.pkce_states.lock().await;
    // Lazy cleanup: remove expired entries while holding the lock
    states.retain(|_, s| s.created_at.elapsed().as_secs() < PKCE_EXPIRY_SECS);
    states.insert(account_id.clone(), pkce_state);

    info!(account_id, "PKCE flow initiated");

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "authorization_url": authorization_url,
            "account_id": account_id,
            "instructions": "Open the URL in a browser, authorize, then paste the code to complete-oauth"
        })
        .to_string(),
    )
}

/// Request body for complete-oauth endpoint.
#[derive(Deserialize)]
struct CompleteOAuthRequest {
    account_id: String,
    code: String,
}

/// POST /admin/accounts/complete-oauth — exchange authorization code for tokens.
///
/// Retrieves the PKCE verifier from the in-memory store, parses the code#state
/// format from the callback, exchanges the code via the token endpoint, stores
/// the credential, and adds the account to the pool.
async fn complete_oauth(
    State(state): State<AdminState>,
    axum::Json(body): axum::Json<CompleteOAuthRequest>,
) -> impl IntoResponse {
    // Retrieve and remove PKCE state
    let pkce_state = {
        let mut states = state.pkce_states.lock().await;
        states.remove(&body.account_id)
    };

    let pkce_state = match pkce_state {
        Some(s) => s,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                serde_json::json!({
                    "error": "no pending OAuth flow for this account_id (expired or not initiated)"
                })
                .to_string(),
            );
        }
    };

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

    // Parse code#state format — the authorization code may contain '#state' suffix
    let authorization_code = body.code.split('#').next().unwrap_or(&body.code);

    // Exchange code for tokens
    let token_response = match anthropic_auth::exchange_code(
        &state.http_client,
        authorization_code,
        &pkce_state.verifier,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(account_id = body.account_id, error = %e, "token exchange failed");
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
    if let Err(e) = credential_store
        .add(body.account_id.clone(), credential)
        .await
    {
        warn!(account_id = body.account_id, error = %e, "failed to store credential");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::json!({
                "error": format!("failed to store credential: {e}")
            })
            .to_string(),
        );
    }

    state.pool.add_account(body.account_id.clone()).await;

    info!(
        account_id = body.account_id,
        "OAuth flow completed, account added to pool"
    );

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "account_id": body.account_id,
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
                "claude-max-expired".to_string(),
                PkceState {
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

        // Verify PKCE state was stored
        let states = pkce_states.lock().await;
        assert!(states.contains_key(account_id));
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
