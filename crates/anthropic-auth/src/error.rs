//! Error types for OAuth authentication operations

/// Errors from OAuth authentication operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("token exchange failed: {0}")]
    TokenExchange(String),

    #[error("invalid credentials: {0}")]
    InvalidCredentials(String),

    #[error("credential parse error: {0}")]
    CredentialParse(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("not found: {0}")]
    NotFound(String),
}

/// Result alias for auth operations.
pub type Result<T> = std::result::Result<T, Error>;
