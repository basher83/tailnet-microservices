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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages_are_descriptive() {
        assert_eq!(
            Error::TailnetAuth.to_string(),
            "Tailnet authentication failed"
        );
        assert_eq!(
            Error::TailnetMachineAuth.to_string(),
            "Tailnet needs machine authorization \u{2014} approve this node in the admin console"
        );
        assert!(
            Error::TailnetConnect("timeout".into())
                .to_string()
                .contains("timeout")
        );
        assert!(
            Error::TailnetNotRunning("socket missing".into())
                .to_string()
                .contains("socket missing")
        );
    }

    #[test]
    fn error_debug_includes_variant_name() {
        let err = Error::TailnetConnect("test error".into());
        let debug = format!("{err:?}");
        assert!(
            debug.contains("TailnetConnect"),
            "Debug output must include variant name, got: {debug}"
        );
    }
}
