//! Error types for pool operations

/// Errors from pool operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("pool exhausted: {0}")]
    PoolExhausted(String),

    #[error("account not found: {0}")]
    NotFound(String),

    #[error("credential store error: {0}")]
    Credential(String),

    #[error("token refresh failed: {0}")]
    RefreshFailed(String),
}

/// Result alias for pool operations.
pub type Result<T> = std::result::Result<T, Error>;
