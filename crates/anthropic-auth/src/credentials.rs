//! Credential storage for OAuth tokens
//!
//! Manages a JSON file mapping account IDs to OAuth credentials. All writes
//! use atomic temp-file + rename to prevent corruption on crash. A tokio Mutex
//! serializes concurrent writes from request-time refresh and background refresh.
//!
//! The credential file is the single source of truth for token data. The pool
//! reads credentials from this store at selection time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::error::{Error, Result};

/// A single account's OAuth credentials.
///
/// `expires` is a unix timestamp in milliseconds (absolute, not a delta).
/// Computed at storage time from `TokenResponse.expires_in` (seconds delta)
/// plus the current time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    /// Always "oauth" for this gateway
    #[serde(rename = "type")]
    pub credential_type: String,
    /// Refresh token for obtaining new access tokens
    pub refresh: String,
    /// Current access token (Bearer token for API calls)
    pub access: String,
    /// Expiration as unix timestamp in milliseconds
    pub expires: u64,
}

/// Thread-safe credential file manager.
///
/// The Mutex serializes all writes. Reads acquire the lock briefly to clone
/// the in-memory state, so request-time reads don't block on background writes.
pub struct CredentialStore {
    path: PathBuf,
    state: Mutex<HashMap<String, Credential>>,
}

impl CredentialStore {
    /// Load credentials from the given file path.
    ///
    /// If the file doesn't exist, creates it as `{}` (cold start with zero
    /// accounts). The pool will report `unhealthy` until accounts are added
    /// via the admin API.
    pub async fn load(path: PathBuf) -> Result<Self> {
        let state = if path.exists() {
            let contents = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| Error::Io(format!("reading credential file: {e}")))?;
            let credentials: HashMap<String, Credential> = serde_json::from_str(&contents)
                .map_err(|e| Error::CredentialParse(format!("parsing credential file: {e}")))?;
            info!(path = %path.display(), accounts = credentials.len(), "loaded credentials");
            credentials
        } else {
            info!(path = %path.display(), "credential file not found, starting with empty store");
            let store = HashMap::new();
            // Create the empty file so future loads don't need the cold-start path
            write_atomic(&path, &store).await?;
            store
        };

        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    /// Persist the current in-memory state to disk.
    ///
    /// Uses atomic write (temp file + rename) to prevent corruption.
    /// File permissions are set to 0600 (owner read/write only).
    pub async fn save(&self) -> Result<()> {
        let state = self.state.lock().await;
        write_atomic(&self.path, &state).await
    }

    /// Get a clone of a specific credential.
    pub async fn get(&self, account_id: &str) -> Option<Credential> {
        let state = self.state.lock().await;
        state.get(account_id).cloned()
    }

    /// List all account IDs.
    pub async fn account_ids(&self) -> Vec<String> {
        let state = self.state.lock().await;
        state.keys().cloned().collect()
    }

    /// Add or replace a credential and persist to disk.
    pub async fn add(&self, account_id: String, credential: Credential) -> Result<()> {
        let mut state = self.state.lock().await;
        state.insert(account_id.clone(), credential);
        debug!(account_id, "added credential");
        write_atomic(&self.path, &state).await
    }

    /// Remove a credential and persist to disk.
    ///
    /// Returns the removed credential if it existed.
    pub async fn remove(&self, account_id: &str) -> Result<Option<Credential>> {
        let mut state = self.state.lock().await;
        let removed = state.remove(account_id);
        if removed.is_some() {
            debug!(account_id, "removed credential");
            write_atomic(&self.path, &state).await?;
        }
        Ok(removed)
    }

    /// Update tokens for an existing account after a refresh.
    ///
    /// Updates the access token, refresh token, and expiration in-memory
    /// and persists to disk. Returns an error if the account doesn't exist.
    pub async fn update_token(
        &self,
        account_id: &str,
        access: String,
        refresh: String,
        expires: u64,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let credential = state.get_mut(account_id).ok_or_else(|| {
            Error::NotFound(format!("account {account_id} not in credential store"))
        })?;
        credential.access = access;
        credential.refresh = refresh;
        credential.expires = expires;
        debug!(account_id, "updated token");
        write_atomic(&self.path, &state).await
    }

    /// Number of stored credentials.
    pub async fn len(&self) -> usize {
        let state = self.state.lock().await;
        state.len()
    }

    /// Whether the store is empty.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Write credentials to a file atomically.
///
/// Writes to a temporary file in the same directory, then renames it over
/// the target. This prevents corruption if the process crashes mid-write.
/// Sets file permissions to 0600 (owner read/write only) since the file
/// contains OAuth tokens.
async fn write_atomic(path: &Path, data: &HashMap<String, Credential>) -> Result<()> {
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| Error::CredentialParse(format!("serializing credentials: {e}")))?;

    let dir = path
        .parent()
        .ok_or_else(|| Error::Io("credential path has no parent directory".into()))?;

    let tmp_path = dir.join(format!(".credentials.tmp.{}", std::process::id()));

    tokio::fs::write(&tmp_path, json.as_bytes())
        .await
        .map_err(|e| Error::Io(format!("writing temp credential file: {e}")))?;

    // Set 0600 permissions (unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(&tmp_path, perms)
            .await
            .map_err(|e| Error::Io(format!("setting credential file permissions: {e}")))?;
    }

    tokio::fs::rename(&tmp_path, path)
        .await
        .map_err(|e| Error::Io(format!("renaming temp credential file: {e}")))?;

    debug!(path = %path.display(), "persisted credentials");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_credential(suffix: &str) -> Credential {
        Credential {
            credential_type: "oauth".into(),
            refresh: format!("rt_{suffix}"),
            access: format!("at_{suffix}"),
            expires: 1735500000000,
        }
    }

    #[tokio::test]
    async fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        // Create store, add credential, save
        let store = CredentialStore::load(path.clone()).await.unwrap();
        store
            .add("claude-max-1".into(), test_credential("1"))
            .await
            .unwrap();

        // Load into a new store instance
        let store2 = CredentialStore::load(path).await.unwrap();
        let cred = store2.get("claude-max-1").await.unwrap();
        assert_eq!(cred.access, "at_1");
        assert_eq!(cred.refresh, "rt_1");
        assert_eq!(cred.credential_type, "oauth");
    }

    #[tokio::test]
    async fn cold_start_creates_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        assert!(!path.exists());
        let store = CredentialStore::load(path.clone()).await.unwrap();
        assert!(store.is_empty().await);
        assert!(path.exists());

        // Verify the file contains valid empty JSON
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: HashMap<String, Credential> = serde_json::from_str(&contents).unwrap();
        assert!(parsed.is_empty());
    }

    #[tokio::test]
    async fn add_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        let store = CredentialStore::load(path).await.unwrap();
        store
            .add("acct-1".into(), test_credential("1"))
            .await
            .unwrap();
        store
            .add("acct-2".into(), test_credential("2"))
            .await
            .unwrap();
        assert_eq!(store.len().await, 2);

        let removed = store.remove("acct-1").await.unwrap();
        assert!(removed.is_some());
        assert_eq!(store.len().await, 1);

        let removed_again = store.remove("acct-1").await.unwrap();
        assert!(removed_again.is_none());
    }

    #[tokio::test]
    async fn update_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        let store = CredentialStore::load(path).await.unwrap();
        store
            .add("acct-1".into(), test_credential("1"))
            .await
            .unwrap();

        store
            .update_token("acct-1", "at_new".into(), "rt_new".into(), 9999999999999)
            .await
            .unwrap();

        let cred = store.get("acct-1").await.unwrap();
        assert_eq!(cred.access, "at_new");
        assert_eq!(cred.refresh, "rt_new");
        assert_eq!(cred.expires, 9999999999999);
    }

    #[tokio::test]
    async fn update_nonexistent_account_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        let store = CredentialStore::load(path).await.unwrap();
        let result = store
            .update_token("nonexistent", "at".into(), "rt".into(), 0)
            .await;

        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        let store = CredentialStore::load(path.clone()).await.unwrap();
        store
            .add("acct-1".into(), test_credential("1"))
            .await
            .unwrap();

        let metadata = tokio::fs::metadata(&path).await.unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential file must be 0600, got {mode:o}");
    }

    #[tokio::test]
    async fn account_ids_returns_all_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        let store = CredentialStore::load(path).await.unwrap();
        store
            .add("b-acct".into(), test_credential("b"))
            .await
            .unwrap();
        store
            .add("a-acct".into(), test_credential("a"))
            .await
            .unwrap();

        let mut ids = store.account_ids().await;
        ids.sort();
        assert_eq!(ids, vec!["a-acct", "b-acct"]);
    }

    #[tokio::test]
    async fn concurrent_writes_dont_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = std::sync::Arc::new(CredentialStore::load(path.clone()).await.unwrap());

        // Spawn multiple concurrent writes
        let mut handles = vec![];
        for i in 0..10 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .add(format!("acct-{i}"), test_credential(&i.to_string()))
                    .await
                    .unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 10 accounts should be present
        assert_eq!(store.len().await, 10);

        // File should be valid JSON
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: HashMap<String, Credential> = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed.len(), 10);
    }
}
