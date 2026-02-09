//! Passthrough provider — wraps existing static header injection behavior.
//!
//! This provider preserves backward compatibility: existing `[[headers]]` config
//! continues to work identically. The proxy delegates to this provider when no
//! `[oauth]` section is present.

use crate::{ErrorClassification, Provider, ProviderHealth};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use tracing::warn;

/// Header injection rule (name + value pair from config).
#[derive(Debug, Clone)]
pub struct HeaderInjection {
    pub name: String,
    pub value: String,
}

/// Static header injection provider — no token management, no body modification.
///
/// Replicates the original proxy behavior: inject configured headers, protect
/// the Authorization header from being overwritten.
pub struct PassthroughProvider {
    headers: Vec<HeaderInjection>,
}

impl PassthroughProvider {
    pub fn new(headers: Vec<HeaderInjection>) -> Self {
        Self { headers }
    }
}

impl Provider for PassthroughProvider {
    fn id(&self) -> &str {
        "passthrough"
    }

    fn needs_body(&self) -> bool {
        false
    }

    fn prepare_request(
        &self,
        headers: &mut HeaderMap,
        _body: &mut serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = crate::Result<()>> + Send + '_>> {
        for injection in &self.headers {
            let name = match HeaderName::from_str(&injection.name) {
                Ok(n) => n,
                Err(e) => {
                    warn!(header = %injection.name, error = %e, "skipping invalid header name");
                    continue;
                }
            };
            if name == reqwest::header::AUTHORIZATION {
                warn!(header = %injection.name, "refusing to overwrite authorization header per spec");
                continue;
            }
            let value = match HeaderValue::from_str(&injection.value) {
                Ok(v) => v,
                Err(e) => {
                    warn!(header = %injection.name, error = %e, "skipping invalid header value");
                    continue;
                }
            };
            headers.insert(name, value);
        }
        Box::pin(async { Ok(()) })
    }

    fn classify_error(&self, _status: u16, _body: &str) -> ErrorClassification {
        // Passthrough has no pool — all errors are transient from its perspective.
        // The existing retry logic in proxy.rs handles timeouts.
        ErrorClassification::Transient
    }

    fn report_error(
        &self,
        _classification: ErrorClassification,
    ) -> Pin<Box<dyn Future<Output = crate::Result<()>> + Send + '_>> {
        // No-op: passthrough has no account state to update.
        Box::pin(async { Ok(()) })
    }

    fn health(&self) -> Pin<Box<dyn Future<Output = ProviderHealth> + Send + '_>> {
        Box::pin(async {
            ProviderHealth {
                status: "healthy".to_string(),
                pool: None,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn injects_configured_headers() {
        let provider = PassthroughProvider::new(vec![
            HeaderInjection {
                name: "anthropic-beta".into(),
                value: "oauth-2025-04-20".into(),
            },
            HeaderInjection {
                name: "x-custom".into(),
                value: "test-value".into(),
            },
        ]);

        let mut headers = HeaderMap::new();
        let mut body = serde_json::Value::Null;
        provider
            .prepare_request(&mut headers, &mut body)
            .await
            .unwrap();

        assert_eq!(headers.get("anthropic-beta").unwrap(), "oauth-2025-04-20");
        assert_eq!(headers.get("x-custom").unwrap(), "test-value");
    }

    #[tokio::test]
    async fn protects_authorization_header() {
        let provider = PassthroughProvider::new(vec![
            HeaderInjection {
                name: "authorization".into(),
                value: "Bearer INJECTED-SHOULD-NOT-APPEAR".into(),
            },
            HeaderInjection {
                name: "anthropic-beta".into(),
                value: "oauth-2025-04-20".into(),
            },
        ]);

        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer sk-real"));
        let mut body = serde_json::Value::Null;
        provider
            .prepare_request(&mut headers, &mut body)
            .await
            .unwrap();

        // Authorization must not be overwritten
        assert_eq!(headers.get("authorization").unwrap(), "Bearer sk-real");
        // Other injections must still work
        assert_eq!(headers.get("anthropic-beta").unwrap(), "oauth-2025-04-20");
    }

    #[tokio::test]
    async fn authorization_not_injected_when_absent() {
        let provider = PassthroughProvider::new(vec![HeaderInjection {
            name: "authorization".into(),
            value: "Bearer INJECTED".into(),
        }]);

        let mut headers = HeaderMap::new();
        let mut body = serde_json::Value::Null;
        provider
            .prepare_request(&mut headers, &mut body)
            .await
            .unwrap();

        // No authorization header should exist
        assert!(headers.get("authorization").is_none());
    }

    #[test]
    fn classify_error_always_returns_transient() {
        let provider = PassthroughProvider::new(vec![]);
        assert_eq!(
            provider.classify_error(429, "rate limit"),
            ErrorClassification::Transient
        );
        assert_eq!(
            provider.classify_error(401, "unauthorized"),
            ErrorClassification::Transient
        );
        assert_eq!(
            provider.classify_error(500, "server error"),
            ErrorClassification::Transient
        );
    }

    #[tokio::test]
    async fn health_returns_healthy_without_pool() {
        let provider = PassthroughProvider::new(vec![]);
        let health = provider.health().await;
        assert_eq!(health.status, "healthy");
        assert!(health.pool.is_none());
    }

    #[test]
    fn id_returns_passthrough() {
        let provider = PassthroughProvider::new(vec![]);
        assert_eq!(provider.id(), "passthrough");
    }

    #[test]
    fn needs_body_returns_false() {
        let provider = PassthroughProvider::new(vec![]);
        assert!(!provider.needs_body());
    }

    #[tokio::test]
    async fn replaces_existing_header_value() {
        let provider = PassthroughProvider::new(vec![HeaderInjection {
            name: "anthropic-beta".into(),
            value: "oauth-2025-04-20".into(),
        }]);

        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", HeaderValue::from_static("old-value"));
        let mut body = serde_json::Value::Null;
        provider
            .prepare_request(&mut headers, &mut body)
            .await
            .unwrap();

        assert_eq!(headers.get("anthropic-beta").unwrap(), "oauth-2025-04-20");
    }

    #[tokio::test]
    async fn skips_invalid_header_name() {
        let provider = PassthroughProvider::new(vec![
            HeaderInjection {
                name: "invalid header name".into(),
                value: "value".into(),
            },
            HeaderInjection {
                name: "x-valid".into(),
                value: "works".into(),
            },
        ]);

        let mut headers = HeaderMap::new();
        let mut body = serde_json::Value::Null;
        provider
            .prepare_request(&mut headers, &mut body)
            .await
            .unwrap();

        assert!(headers.get("invalid header name").is_none());
        assert_eq!(headers.get("x-valid").unwrap(), "works");
    }
}
