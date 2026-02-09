//! Provider abstraction for upstream API authentication
//!
//! Defines the `Provider` trait that decouples proxy logic from authentication
//! strategy. PassthroughProvider wraps the existing static header injection;
//! future providers (e.g. AnthropicOAuthProvider) implement the same trait
//! with token management, body modification, and error classification.

pub mod passthrough;

pub use passthrough::PassthroughProvider;

use serde::Serialize;
use std::future::Future;
use std::pin::Pin;

/// Classification of upstream errors to determine retry/failover strategy.
///
/// Passthrough mode always returns Transient (no pool to failover).
/// OAuth mode uses this to drive account state transitions:
/// - QuotaExceeded triggers cooldown and failover to next account
/// - Permanent disables the account entirely
/// - Transient uses existing retry logic (no pool action)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClassification {
    /// Retryable on the same account (timeouts, 5xx)
    Transient,
    /// 5-hour quota exhausted, failover to next account
    QuotaExceeded,
    /// Invalid credentials (401/403), disable account
    Permanent,
}

/// Health status reported by a provider for the /health endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    /// Overall status: "healthy", "degraded", or "unhealthy"
    pub status: String,
    /// Provider-specific details (e.g. pool account counts in OAuth mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<serde_json::Value>,
}

/// Errors from provider operations (token refresh, credential storage, etc.)
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("pool exhausted: {0}")]
    PoolExhausted(String),

    #[error("internal provider error: {0}")]
    Internal(String),
}

/// Result alias for provider operations.
pub type Result<T> = std::result::Result<T, ProviderError>;

/// Abstraction over upstream API authentication strategies.
///
/// The proxy delegates all auth concerns to the provider:
/// - `prepare_request` injects/modifies headers and optionally the body
/// - `classify_error` determines retry vs failover vs disable
/// - `health` reports provider-specific status for the health endpoint
///
/// Uses `Pin<Box<dyn Future>>` return types for dyn-compatibility (`Arc<dyn Provider>`).
pub trait Provider: Send + Sync {
    /// Identifier for logging and health reporting (e.g. "passthrough", "anthropic")
    fn id(&self) -> &str;

    /// Whether this provider needs to inspect/modify the request body.
    /// Passthrough returns false (opaque byte forwarding, zero overhead).
    /// OAuth returns true (deserialize JSON, inject system prompt, serialize back).
    fn needs_body(&self) -> bool;

    /// Inject authentication headers and optionally modify the request body.
    ///
    /// Called before forwarding each request to upstream. The provider may:
    /// - Add/replace headers (e.g. Authorization, anthropic-beta)
    /// - Modify the JSON body (e.g. inject system prompt)
    ///
    /// Returns the account ID used for this request (for error reporting), or
    /// None if the provider doesn't use accounts (passthrough mode).
    ///
    /// If `needs_body()` is false, `body` will be `Value::Null` and should be ignored.
    fn prepare_request<'a>(
        &'a self,
        headers: &'a mut reqwest::header::HeaderMap,
        body: &'a mut serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + Send + 'a>>;

    /// Classify an upstream error response to determine the retry strategy.
    fn classify_error(&self, status: u16, body: &str) -> ErrorClassification;

    /// Report an error classification back to the provider for state management.
    /// OAuth mode uses this to transition accounts (cooldown, disable).
    /// Passthrough mode is a no-op.
    ///
    /// `account_id` identifies which account experienced the error. Providers
    /// without account pools ignore this parameter.
    fn report_error(
        &self,
        account_id: &str,
        classification: ErrorClassification,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Provider health for the /health endpoint.
    fn health(&self) -> Pin<Box<dyn Future<Output = ProviderHealth> + Send + '_>>;
}
