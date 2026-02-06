//! Service-specific error types

use thiserror::Error;

/// OAuth Proxy errors per spec (specs/oauth-proxy.md "Error Handling" section).
/// Variants are constructed as each integration path is implemented.
#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Tailnet authentication failed")]
    TailnetAuth,

    #[error("Tailnet connection failed: {0}")]
    TailnetConnect(String),

    #[error("Failed to bind listener: {0}")]
    ListenerBind(String),

    #[error("Upstream timeout after {0}s")]
    UpstreamTimeout(u64),

    #[error("Upstream error: {0}")]
    Upstream(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),
}

/// Result alias
#[allow(dead_code)]
pub type Result<T> = std::result::Result<T, Error>;
