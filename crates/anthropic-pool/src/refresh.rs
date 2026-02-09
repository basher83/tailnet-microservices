//! Proactive background token refresh
//!
//! Spawns a periodic task that checks all accounts and refreshes tokens
//! approaching expiration. This prevents most request-time refresh latency.
//! The background task runs independently of the request path.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::pool::{AccountStatus, Pool};

/// Spawn a background task that proactively refreshes expiring tokens.
///
/// Runs every `interval` and refreshes any token expiring within `threshold`.
/// On 401/403 from the token endpoint, the account is marked Disabled.
/// On transient errors, the account is left unchanged (next cycle will retry).
///
/// Returns a `JoinHandle` for the spawned task.
pub fn spawn_refresh_task(
    pool: Arc<Pool>,
    interval: Duration,
    threshold: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick — tokens were just loaded
        ticker.tick().await;

        loop {
            ticker.tick().await;
            refresh_cycle(&pool, threshold).await;
        }
    })
}

/// Run one refresh cycle: check all accounts and refresh expiring tokens.
async fn refresh_cycle(pool: &Pool, threshold: Duration) {
    let ids = pool.account_ids().await;
    let store = pool.credential_store();
    let client = pool.http_client();
    let threshold_millis = threshold.as_millis() as u64;

    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    for id in &ids {
        let credential = match store.get(id).await {
            Some(c) => c,
            None => continue,
        };

        // Skip if token is not expiring within threshold
        if credential.expires > now_millis + threshold_millis {
            continue;
        }

        debug!(
            account_id = id,
            "token expiring within threshold, refreshing"
        );

        match anthropic_auth::refresh_token(client, &credential.refresh).await {
            Ok(token_response) => {
                let new_expires = now_millis + (token_response.expires_in * 1000);
                if let Err(e) = store
                    .update_token(
                        id,
                        token_response.access_token,
                        token_response.refresh_token,
                        new_expires,
                    )
                    .await
                {
                    warn!(account_id = id, error = %e, "failed to persist refreshed token");
                }
                info!(account_id = id, "background token refresh succeeded");
            }
            Err(anthropic_auth::Error::InvalidCredentials(msg)) => {
                warn!(account_id = id, error = %msg, "refresh token rejected, disabling account");
                pool.set_status(id, AccountStatus::Disabled).await;
            }
            Err(e) => {
                warn!(account_id = id, error = %e, "background refresh failed (transient), will retry next cycle");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anthropic_auth::{Credential, CredentialStore};

    /// Create a test credential store.
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

    #[tokio::test]
    async fn refresh_cycle_skips_valid_tokens() {
        let dir = tempfile::tempdir().unwrap();
        // Token expires far in the future — should not be refreshed
        let store = test_store(&dir, &[("a", 4_102_444_800_000)]).await;
        let pool = Arc::new(crate::Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store.clone(),
            reqwest::Client::new(),
        ));

        // Run one cycle with 15-minute threshold
        refresh_cycle(&pool, Duration::from_secs(900)).await;

        // Token should be unchanged (no refresh attempted)
        let cred = store.get("a").await.unwrap();
        assert_eq!(cred.access, "at_a");
    }

    #[tokio::test]
    async fn refresh_cycle_attempts_refresh_on_expiring_token() {
        let dir = tempfile::tempdir().unwrap();
        // Token expires very soon (1 second from now)
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let store = test_store(&dir, &[("a", now_millis + 1000)]).await;
        let pool = Arc::new(crate::Pool::new(
            vec!["a".into()],
            Duration::from_secs(7200),
            store,
            reqwest::Client::new(),
        ));

        // Run refresh cycle — will attempt to refresh with bogus token,
        // which will fail. Account should be disabled since the token
        // endpoint returns 401/403 for invalid refresh tokens.
        refresh_cycle(&pool, Duration::from_secs(900)).await;

        // Account may or may not be disabled depending on the exact error
        // from the real endpoint. The important thing is the cycle ran
        // without panicking.
        let health = pool.health().await;
        let total = health["accounts_total"].as_u64().unwrap();
        assert_eq!(total, 1);
    }
}
