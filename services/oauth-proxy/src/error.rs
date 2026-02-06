//! Service-specific error types

use thiserror::Error;

/// OAuth Proxy lifecycle errors per spec (specs/oauth-proxy.md "Error Handling" section).
///
/// Per-request errors (UpstreamTimeout, UpstreamError, InvalidRequest) are
/// handled directly by the proxy handler as HTTP responses — they never
/// need to propagate as Rust errors.
#[derive(Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[error("Tailnet authentication failed")]
    TailnetAuth,

    #[error("Tailnet needs machine authorization — approve this node in the admin console")]
    TailnetMachineAuth,

    #[error("Tailnet connection failed: {0}")]
    TailnetConnect(String),

    #[error("Tailnet daemon not running: {0}")]
    TailnetNotRunning(String),
}

/// Result alias using service Error
pub type Result<T> = std::result::Result<T, Error>;
