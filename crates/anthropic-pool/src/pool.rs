//! Pool state machine and round-robin account selection
//!
//! The pool holds per-account status (Available, CoolingDown, Disabled) and selects
//! accounts round-robin. The credential store is the single source of truth for
//! token data; the pool reads credentials at selection time.
//!
//! Cooldown transitions happen automatically: when a CoolingDown account is checked
//! and its cooldown has expired, it transitions back to Available without explicit action.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anthropic_auth::CredentialStore;
use provider::ErrorClassification;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

/// Runtime status of a pool account.
///
/// Transitions:
/// - Available → CoolingDown (quota exhausted 429)
/// - Available → Disabled (401/403 permanent error)
/// - CoolingDown → Available (cooldown expired)
/// - CoolingDown → Disabled (refresh failure while cooling)
/// - Disabled → (removed by admin)
#[derive(Debug, Clone)]
pub enum AccountStatus {
    Available,
    CoolingDown { until: Instant },
    Disabled,
}

impl AccountStatus {
    /// Status label for health/logging.
    pub fn label(&self) -> &'static str {
        match self {
            AccountStatus::Available => "available",
            AccountStatus::CoolingDown { .. } => "cooling_down",
            AccountStatus::Disabled => "disabled",
        }
    }
}

/// A selected account with its access token, ready for a request.
#[derive(Debug)]
pub struct SelectedAccount {
    pub id: String,
    pub access_token: String,
}

/// Subscription pool managing multiple OAuth accounts.
///
/// Uses an `AtomicUsize` for the round-robin index and `RwLock` for the account
/// list and status map. The credential store is shared via `Arc` and provides
/// the token data.
pub struct Pool {
    account_ids: RwLock<Vec<String>>,
    statuses: RwLock<HashMap<String, AccountStatus>>,
    next_index: AtomicUsize,
    cooldown_duration: Duration,
    credential_store: std::sync::Arc<CredentialStore>,
    http_client: reqwest::Client,
}

impl Pool {
    /// Create a new pool backed by the given credential store.
    ///
    /// `account_ids` is the initial list of accounts to manage. Each must have
    /// a corresponding entry in the credential store. Accounts start as Available.
    pub fn new(
        account_ids: Vec<String>,
        cooldown_duration: Duration,
        credential_store: std::sync::Arc<CredentialStore>,
        http_client: reqwest::Client,
    ) -> Self {
        let statuses: HashMap<String, AccountStatus> = account_ids
            .iter()
            .map(|id| (id.clone(), AccountStatus::Available))
            .collect();
        info!(accounts = account_ids.len(), "pool initialized");
        Self {
            account_ids: RwLock::new(account_ids),
            statuses: RwLock::new(statuses),
            next_index: AtomicUsize::new(0),
            cooldown_duration,
            credential_store,
            http_client,
        }
    }

    /// Select the next available account via round-robin.
    ///
    /// Scans all accounts starting from `next_index`. Expired cooldowns are
    /// transitioned to Available automatically. If a selected account's token
    /// expires within 60 seconds, attempts an inline refresh; on failure, the
    /// account is disabled and the scan continues.
    ///
    /// Returns `PoolExhausted` with pool counts if no account is available.
    pub async fn select(&self) -> Result<SelectedAccount> {
        let ids = self.account_ids.read().await;
        let n = ids.len();
        if n == 0 {
            return Err(Error::PoolExhausted(
                self.exhausted_message(0, 0, 0, 0).await,
            ));
        }

        let start = self.next_index.fetch_add(1, Ordering::Relaxed) % n;

        for offset in 0..n {
            let idx = (start + offset) % n;
            let id = &ids[idx];

            // Check and possibly transition status
            let available = {
                let mut statuses = self.statuses.write().await;
                let status = statuses.get(id);
                match status {
                    Some(AccountStatus::Available) => true,
                    Some(AccountStatus::CoolingDown { until }) => {
                        if Instant::now() >= *until {
                            info!(account_id = id, "cooldown expired, account available again");
                            statuses.insert(id.clone(), AccountStatus::Available);
                            true
                        } else {
                            false
                        }
                    }
                    Some(AccountStatus::Disabled) | None => false,
                }
            };

            if !available {
                continue;
            }

            // Get credential from store
            let credential = match self.credential_store.get(id).await {
                Some(c) => c,
                None => {
                    warn!(
                        account_id = id,
                        "account in pool but not in credential store, disabling"
                    );
                    self.statuses
                        .write()
                        .await
                        .insert(id.clone(), AccountStatus::Disabled);
                    continue;
                }
            };

            // Request-time refresh: if token expires within 60 seconds, refresh inline
            let now_millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let refresh_threshold_millis = 60_000;

            if credential.expires <= now_millis + refresh_threshold_millis {
                debug!(
                    account_id = id,
                    "token expiring soon, attempting inline refresh"
                );
                match anthropic_auth::refresh_token(&self.http_client, &credential.refresh).await {
                    Ok(token_response) => {
                        let new_expires = now_millis + (token_response.expires_in * 1000);
                        if let Err(e) = self
                            .credential_store
                            .update_token(
                                id,
                                token_response.access_token.clone(),
                                token_response.refresh_token,
                                new_expires,
                            )
                            .await
                        {
                            warn!(account_id = id, error = %e, "failed to persist refreshed token");
                        }
                        info!(account_id = id, "inline token refresh succeeded");
                        return Ok(SelectedAccount {
                            id: id.clone(),
                            access_token: token_response.access_token,
                        });
                    }
                    Err(e) => {
                        warn!(account_id = id, error = %e, "inline refresh failed, disabling account");
                        self.statuses
                            .write()
                            .await
                            .insert(id.clone(), AccountStatus::Disabled);
                        continue;
                    }
                }
            }

            return Ok(SelectedAccount {
                id: id.clone(),
                access_token: credential.access,
            });
        }

        // All accounts exhausted
        let (total, available, cooling, disabled) = self.count_statuses().await;
        Err(Error::PoolExhausted(
            self.exhausted_message(total, available, cooling, disabled)
                .await,
        ))
    }

    /// Report an error classification for an account, triggering state transitions.
    ///
    /// - QuotaExceeded → CoolingDown for cooldown_duration
    /// - Permanent → Disabled
    /// - Transient → no change
    pub async fn report_error(&self, account_id: &str, classification: ErrorClassification) {
        let mut statuses = self.statuses.write().await;
        match classification {
            ErrorClassification::QuotaExceeded => {
                let until = Instant::now() + self.cooldown_duration;
                info!(
                    account_id,
                    cooldown_secs = self.cooldown_duration.as_secs(),
                    "account entering cooldown (quota exhausted)"
                );
                statuses.insert(account_id.to_string(), AccountStatus::CoolingDown { until });
            }
            ErrorClassification::Permanent => {
                warn!(account_id, "account disabled (permanent error)");
                statuses.insert(account_id.to_string(), AccountStatus::Disabled);
            }
            ErrorClassification::Transient => {
                debug!(account_id, "transient error, no pool action");
            }
        }
    }

    /// Add a new account to the pool. Starts as Available.
    pub async fn add_account(&self, account_id: String) {
        let mut ids = self.account_ids.write().await;
        if !ids.contains(&account_id) {
            ids.push(account_id.clone());
        }
        self.statuses
            .write()
            .await
            .insert(account_id.clone(), AccountStatus::Available);
        info!(account_id, "account added to pool");
    }

    /// Remove an account from the pool.
    pub async fn remove_account(&self, account_id: &str) {
        let mut ids = self.account_ids.write().await;
        ids.retain(|id| id != account_id);
        self.statuses.write().await.remove(account_id);
        info!(account_id, "account removed from pool");
    }

    /// Pool health summary for the health endpoint.
    ///
    /// Returns a JSON value with per-account status and overall pool health.
    /// Status mapping: all available → healthy, some available → degraded,
    /// none available → unhealthy.
    pub async fn health(&self) -> serde_json::Value {
        let ids = self.account_ids.read().await;
        let statuses = self.statuses.read().await;
        let now = Instant::now();

        let mut accounts = Vec::new();
        let mut available_count = 0usize;
        let mut cooling_count = 0usize;
        let mut disabled_count = 0usize;

        for id in ids.iter() {
            let status = statuses.get(id);
            match status {
                Some(AccountStatus::Available) => {
                    available_count += 1;
                    accounts.push(serde_json::json!({
                        "id": id,
                        "status": "available"
                    }));
                }
                Some(AccountStatus::CoolingDown { until }) => {
                    let remaining = if *until > now {
                        (*until - now).as_secs()
                    } else {
                        0
                    };
                    cooling_count += 1;
                    accounts.push(serde_json::json!({
                        "id": id,
                        "status": "cooling_down",
                        "cooldown_remaining_secs": remaining
                    }));
                }
                Some(AccountStatus::Disabled) => {
                    disabled_count += 1;
                    accounts.push(serde_json::json!({
                        "id": id,
                        "status": "disabled"
                    }));
                }
                None => {
                    disabled_count += 1;
                    accounts.push(serde_json::json!({
                        "id": id,
                        "status": "disabled"
                    }));
                }
            }
        }

        let total = ids.len();
        let pool_status = if available_count == total && total > 0 {
            "healthy"
        } else if available_count > 0 {
            "degraded"
        } else {
            "unhealthy"
        };

        serde_json::json!({
            "status": pool_status,
            "accounts_total": total,
            "accounts_available": available_count,
            "accounts_cooling_down": cooling_count,
            "accounts_disabled": disabled_count,
            "accounts": accounts
        })
    }

    /// Get the credential store reference (for background refresh).
    pub fn credential_store(&self) -> &std::sync::Arc<CredentialStore> {
        &self.credential_store
    }

    /// Get the HTTP client reference (for background refresh).
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Get a snapshot of all account IDs.
    pub async fn account_ids(&self) -> Vec<String> {
        self.account_ids.read().await.clone()
    }

    /// Set an account's status directly (used by background refresh on failure).
    pub async fn set_status(&self, account_id: &str, status: AccountStatus) {
        self.statuses
            .write()
            .await
            .insert(account_id.to_string(), status);
    }

    /// Count accounts by status.
    async fn count_statuses(&self) -> (usize, usize, usize, usize) {
        let ids = self.account_ids.read().await;
        let statuses = self.statuses.read().await;
        let now = Instant::now();
        let total = ids.len();
        let mut available = 0usize;
        let mut cooling = 0usize;
        let mut disabled = 0usize;

        for id in ids.iter() {
            match statuses.get(id) {
                Some(AccountStatus::Available) => available += 1,
                Some(AccountStatus::CoolingDown { until }) => {
                    if now >= *until {
                        available += 1;
                    } else {
                        cooling += 1;
                    }
                }
                Some(AccountStatus::Disabled) | None => disabled += 1,
            }
        }
        (total, available, cooling, disabled)
    }

    /// Build the exhausted error message JSON.
    async fn exhausted_message(
        &self,
        total: usize,
        available: usize,
        cooling: usize,
        disabled: usize,
    ) -> String {
        serde_json::json!({
            "error": {
                "type": "pool_exhausted",
                "message": "All accounts exhausted",
                "pool": {
                    "accounts_total": total,
                    "accounts_available": available,
                    "accounts_cooling_down": cooling,
                    "accounts_disabled": disabled
                }
            }
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anthropic_auth::Credential;
    use std::sync::Arc;

    /// Create a credential store with test accounts.
    async fn test_store(dir: &tempfile::TempDir, accounts: &[(&str, u64)]) -> Arc<CredentialStore> {
        let path = dir.path().join("credentials.json");
        let store = CredentialStore::load(path).await.unwrap();
        for (id, expires) in accounts {
            store
                .add(
                    id.to_string(),
                    Credential {
                        credential_type: "oauth".into(),
                        refresh: format!("rt_{id}"),
                        access: format!("at_{id}"),
                        expires: *expires,
                    },
                )
                .await
                .unwrap();
        }
        Arc::new(store)
    }

    /// Expiration far in the future (year 2100).
    fn future_expiry() -> u64 {
        4_102_444_800_000
    }

    /// Expiration in the past.
    fn past_expiry() -> u64 {
        1_000_000_000
    }

    #[tokio::test]
    async fn round_robin_cycles_through_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry()), ("b", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into(), "b".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        let s1 = pool.select().await.unwrap();
        let s2 = pool.select().await.unwrap();
        let s3 = pool.select().await.unwrap();

        assert_eq!(s1.id, "a");
        assert_eq!(s2.id, "b");
        assert_eq!(s3.id, "a");
    }

    #[tokio::test]
    async fn skips_cooling_down_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(
            &dir,
            &[
                ("a", future_expiry()),
                ("b", future_expiry()),
                ("c", future_expiry()),
            ],
        )
        .await;
        let pool = Pool::new(
            vec!["a".into(), "b".into(), "c".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        // Put "a" in cooldown
        pool.report_error("a", ErrorClassification::QuotaExceeded)
            .await;

        // Selections should skip "a"
        let s1 = pool.select().await.unwrap();
        let s2 = pool.select().await.unwrap();
        assert_ne!(s1.id, "a");
        assert_ne!(s2.id, "a");
    }

    #[tokio::test]
    async fn skips_disabled_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry()), ("b", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into(), "b".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::Permanent).await;

        // All selections should be "b"
        for _ in 0..5 {
            let s = pool.select().await.unwrap();
            assert_eq!(s.id, "b");
        }
    }

    #[tokio::test]
    async fn expired_cooldown_transitions_to_available() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(0), // Zero cooldown for testing
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::QuotaExceeded)
            .await;

        // Cooldown is 0 seconds, so it should be expired immediately
        // (Instant::now() >= until since until = now + 0)
        // Small sleep to ensure time advances past the instant
        tokio::time::sleep(Duration::from_millis(1)).await;

        let s = pool.select().await.unwrap();
        assert_eq!(s.id, "a");
    }

    #[tokio::test]
    async fn all_exhausted_returns_error_with_counts() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry()), ("b", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into(), "b".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::QuotaExceeded)
            .await;
        pool.report_error("b", ErrorClassification::Permanent).await;

        let err = pool.select().await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("pool_exhausted"), "error: {msg}");

        let json: serde_json::Value =
            serde_json::from_str(msg.strip_prefix("pool exhausted: ").unwrap_or(&msg)).unwrap();
        assert_eq!(json["error"]["pool"]["accounts_total"], 2);
        assert_eq!(json["error"]["pool"]["accounts_available"], 0);
        assert_eq!(json["error"]["pool"]["accounts_cooling_down"], 1);
        assert_eq!(json["error"]["pool"]["accounts_disabled"], 1);
    }

    #[tokio::test]
    async fn empty_pool_returns_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[]).await;
        let pool = Pool::new(
            vec![],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        let err = pool.select().await.unwrap_err();
        assert!(err.to_string().contains("pool_exhausted"));
    }

    #[tokio::test]
    async fn report_error_quota_sets_cooling_down() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::QuotaExceeded)
            .await;

        let health = pool.health().await;
        assert_eq!(health["accounts_cooling_down"], 1);
    }

    #[tokio::test]
    async fn report_error_permanent_sets_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::Permanent).await;

        let health = pool.health().await;
        assert_eq!(health["accounts_disabled"], 1);
    }

    #[tokio::test]
    async fn report_error_transient_no_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::Transient).await;

        let health = pool.health().await;
        assert_eq!(health["accounts_available"], 1);
    }

    #[tokio::test]
    async fn add_and_remove_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.add_account("b".into()).await;
        let ids = pool.account_ids().await;
        assert_eq!(ids.len(), 2);

        pool.remove_account("a").await;
        let ids = pool.account_ids().await;
        assert_eq!(ids, vec!["b"]);
    }

    #[tokio::test]
    async fn add_account_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.add_account("a".into()).await;
        let ids = pool.account_ids().await;
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn health_all_available_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry()), ("b", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into(), "b".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        let health = pool.health().await;
        assert_eq!(health["status"], "healthy");
        assert_eq!(health["accounts_total"], 2);
        assert_eq!(health["accounts_available"], 2);
    }

    #[tokio::test]
    async fn health_some_available_is_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry()), ("b", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into(), "b".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::QuotaExceeded)
            .await;

        let health = pool.health().await;
        assert_eq!(health["status"], "degraded");
    }

    #[tokio::test]
    async fn health_none_available_is_unhealthy() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::Permanent).await;

        let health = pool.health().await;
        assert_eq!(health["status"], "unhealthy");
    }

    #[tokio::test]
    async fn health_empty_pool_is_unhealthy() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[]).await;
        let pool = Pool::new(
            vec![],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        let health = pool.health().await;
        assert_eq!(health["status"], "unhealthy");
        assert_eq!(health["accounts_total"], 0);
    }

    #[tokio::test]
    async fn health_cooling_down_shows_remaining_secs() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("a", future_expiry())]).await;
        let pool = Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        pool.report_error("a", ErrorClassification::QuotaExceeded)
            .await;

        let health = pool.health().await;
        let accounts = health["accounts"].as_array().unwrap();
        let acct = &accounts[0];
        assert_eq!(acct["status"], "cooling_down");
        // Should have a positive cooldown_remaining_secs
        let remaining = acct["cooldown_remaining_secs"].as_u64().unwrap();
        assert!(remaining > 0, "remaining should be > 0, got {remaining}");
    }

    #[tokio::test]
    async fn select_returns_access_token_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir, &[("acct-1", future_expiry())]).await;
        let pool = Pool::new(
            vec!["acct-1".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        let selected = pool.select().await.unwrap();
        assert_eq!(selected.id, "acct-1");
        assert_eq!(selected.access_token, "at_acct-1");
    }

    #[tokio::test]
    async fn select_disables_account_missing_from_store() {
        let dir = tempfile::tempdir().unwrap();
        // Pool knows about "ghost" but store doesn't have it
        let store = test_store(&dir, &[("real", future_expiry())]).await;
        let pool = Pool::new(
            vec!["ghost".into(), "real".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        // First select should skip "ghost" (disabled) and return "real"
        let s = pool.select().await.unwrap();
        assert_eq!(s.id, "real");

        // Verify ghost is now disabled
        let health = pool.health().await;
        assert_eq!(health["accounts_disabled"], 1);
    }

    #[tokio::test]
    async fn select_with_expired_token_attempts_refresh() {
        // Token with past expiry triggers inline refresh, which will fail
        // (no real token endpoint), causing the account to be disabled
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(
            &dir,
            &[("expired", past_expiry()), ("valid", future_expiry())],
        )
        .await;
        let pool = Pool::new(
            vec!["expired".into(), "valid".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        );

        // Should fail refresh on "expired", disable it, then select "valid"
        let s = pool.select().await.unwrap();
        assert_eq!(s.id, "valid");

        // "expired" should now be disabled
        let health = pool.health().await;
        assert_eq!(health["accounts_disabled"], 1);
        assert_eq!(health["accounts_available"], 1);
    }
}
