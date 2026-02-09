//! Anthropic OAuth provider — pool-backed token injection and body modification.
//!
//! Implements the Provider trait using the subscription pool for account selection,
//! token injection, beta header merging, and system prompt injection. This is the
//! OAuth pool mode counterpart to PassthroughProvider.

use anthropic_auth::REQUIRED_SYSTEM_PROMPT_PREFIX;
use anthropic_pool::Pool;
use provider::{ErrorClassification, Provider, ProviderError, ProviderHealth};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tracing::{debug, warn};

/// Required anthropic-beta flags for OAuth mode. These are always injected and
/// merged with any client-provided beta flags (deduplicated).
const REQUIRED_BETA_FLAGS: &[&str] = &[
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
    "context-management-2025-06-27",
];

/// User-Agent header value matching the Claude CLI identity.
const USER_AGENT: &str = "claude-cli/2.0.76 (external, sdk-cli)";

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// OAuth provider backed by a subscription pool.
///
/// Selects accounts round-robin, injects Bearer tokens, merges anthropic-beta
/// flags, and injects the required system prompt prefix for non-Haiku models.
pub struct AnthropicOAuthProvider {
    pool: Arc<Pool>,
}

impl AnthropicOAuthProvider {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }
}

impl Provider for AnthropicOAuthProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn needs_body(&self) -> bool {
        true
    }

    fn prepare_request<'a>(
        &'a self,
        headers: &'a mut HeaderMap,
        body: &'a mut serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = provider::Result<Option<String>>> + Send + 'a>> {
        Box::pin(async move {
            let selected = self.pool.select().await.map_err(|e| match e {
                anthropic_pool::Error::PoolExhausted(msg) => ProviderError::PoolExhausted(msg),
                other => ProviderError::Internal(other.to_string()),
            })?;

            // Strip any client-provided Authorization header — OAuth mode manages
            // its own credentials, client auth is not forwarded.
            headers.remove(reqwest::header::AUTHORIZATION);

            // Inject Bearer token from the selected account
            headers.insert(
                reqwest::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", selected.access_token))
                    .map_err(|e| ProviderError::Internal(format!("invalid token value: {e}")))?,
            );

            // Merge anthropic-beta flags: combine required flags with any
            // client-provided flags, deduplicating.
            merge_beta_headers(headers);

            // Inject required headers
            headers.insert(
                HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
                HeaderValue::from_static("true"),
            );
            headers.insert(
                reqwest::header::USER_AGENT,
                HeaderValue::from_static(USER_AGENT),
            );
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static(ANTHROPIC_VERSION),
            );

            // System prompt injection for non-Haiku models
            inject_system_prompt(body);

            Ok(Some(selected.id))
        })
    }

    fn classify_error(&self, status: u16, body: &str) -> ErrorClassification {
        anthropic_pool::classify_status(status, body)
    }

    fn report_error(
        &self,
        account_id: &str,
        classification: ErrorClassification,
    ) -> Pin<Box<dyn Future<Output = provider::Result<()>> + Send + '_>> {
        let account_id = account_id.to_string();
        Box::pin(async move {
            self.pool.report_error(&account_id, classification).await;
            Ok(())
        })
    }

    fn health(&self) -> Pin<Box<dyn Future<Output = ProviderHealth> + Send + '_>> {
        Box::pin(async move {
            let pool_health = self.pool.health().await;
            let status = pool_health
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unhealthy")
                .to_string();
            ProviderHealth {
                status,
                pool: Some(pool_health),
            }
        })
    }
}

/// Merge required anthropic-beta flags with any client-provided flags.
///
/// Reads the existing `anthropic-beta` header, splits by comma, combines with
/// the required set, deduplicates, and writes back as a single comma-separated
/// header value.
fn merge_beta_headers(headers: &mut HeaderMap) {
    let mut flags: Vec<String> = REQUIRED_BETA_FLAGS.iter().map(|s| s.to_string()).collect();

    if let Some(existing) = headers.get("anthropic-beta")
        && let Ok(existing_str) = existing.to_str()
    {
        for flag in existing_str.split(',') {
            let trimmed = flag.trim().to_string();
            if !trimmed.is_empty() && !flags.contains(&trimmed) {
                flags.push(trimmed);
            }
        }
    }

    let merged = flags.join(",");
    match HeaderValue::from_str(&merged) {
        Ok(v) => {
            headers.insert(HeaderName::from_static("anthropic-beta"), v);
        }
        Err(e) => {
            warn!(error = %e, "failed to construct merged anthropic-beta header");
        }
    }
}

/// Extract the model name from a request body JSON object.
fn extract_model(body: &serde_json::Value) -> Option<&str> {
    body.get("model").and_then(|m| m.as_str())
}

/// Inject the required system prompt prefix for non-Haiku models.
///
/// Rules:
/// - Haiku models: skip entirely (no system prompt required)
/// - No `system` field: create with required prefix
/// - Existing `system` without prefix: prepend prefix + space + existing
/// - Existing `system` already has prefix: no modification
fn inject_system_prompt(body: &mut serde_json::Value) {
    let model = match extract_model(body) {
        Some(m) => m.to_lowercase(),
        None => return,
    };

    // Haiku models don't require system prompt injection
    if model.contains("haiku") {
        debug!(model = %model, "skipping system prompt injection for haiku model");
        return;
    }

    match body.get("system") {
        None => {
            body["system"] = serde_json::Value::String(REQUIRED_SYSTEM_PROMPT_PREFIX.to_string());
            debug!("injected system prompt (no existing system field)");
        }
        Some(existing) => {
            if let Some(existing_str) = existing.as_str()
                && !existing_str.starts_with(REQUIRED_SYSTEM_PROMPT_PREFIX)
            {
                body["system"] = serde_json::Value::String(format!(
                    "{REQUIRED_SYSTEM_PROMPT_PREFIX} {existing_str}"
                ));
                debug!("prepended system prompt prefix to existing system field");
            }
            // Already has prefix or non-string system field: leave as-is
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Beta header merge tests ---

    #[test]
    fn merge_beta_no_client_headers() {
        let mut headers = HeaderMap::new();
        merge_beta_headers(&mut headers);
        let beta = headers.get("anthropic-beta").unwrap().to_str().unwrap();
        assert_eq!(
            beta,
            "oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27"
        );
    }

    #[test]
    fn merge_beta_client_with_overlap() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("oauth-2025-04-20,custom-feature-2025-01-01"),
        );
        merge_beta_headers(&mut headers);
        let beta = headers.get("anthropic-beta").unwrap().to_str().unwrap();
        // Required flags first, then client extras (no duplicate oauth-2025-04-20)
        assert!(beta.contains("oauth-2025-04-20"));
        assert!(beta.contains("interleaved-thinking-2025-05-14"));
        assert!(beta.contains("context-management-2025-06-27"));
        assert!(beta.contains("custom-feature-2025-01-01"));
        // Count occurrences of oauth-2025-04-20 — should be exactly 1
        assert_eq!(beta.matches("oauth-2025-04-20").count(), 1);
    }

    #[test]
    fn merge_beta_client_with_extra() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("custom-feature-2025-01-01"),
        );
        merge_beta_headers(&mut headers);
        let beta = headers.get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(beta.contains("oauth-2025-04-20"));
        assert!(beta.contains("interleaved-thinking-2025-05-14"));
        assert!(beta.contains("context-management-2025-06-27"));
        assert!(beta.contains("custom-feature-2025-01-01"));
    }

    #[test]
    fn merge_beta_empty_client_header() {
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", HeaderValue::from_static(""));
        merge_beta_headers(&mut headers);
        let beta = headers.get("anthropic-beta").unwrap().to_str().unwrap();
        assert_eq!(
            beta,
            "oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27"
        );
    }

    // --- Model extraction tests ---

    #[test]
    fn extract_model_present() {
        let body = serde_json::json!({"model": "claude-sonnet-4-20250514", "messages": []});
        assert_eq!(extract_model(&body), Some("claude-sonnet-4-20250514"));
    }

    #[test]
    fn extract_model_missing() {
        let body = serde_json::json!({"messages": []});
        assert_eq!(extract_model(&body), None);
    }

    #[test]
    fn extract_model_not_string() {
        let body = serde_json::json!({"model": 42});
        assert_eq!(extract_model(&body), None);
    }

    // --- System prompt injection tests ---

    #[test]
    fn inject_no_system_field() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "hello"}]
        });
        inject_system_prompt(&mut body);
        assert_eq!(
            body["system"].as_str().unwrap(),
            REQUIRED_SYSTEM_PROMPT_PREFIX
        );
    }

    #[test]
    fn inject_existing_system_without_prefix() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful assistant.",
            "messages": []
        });
        inject_system_prompt(&mut body);
        let system = body["system"].as_str().unwrap();
        assert!(system.starts_with(REQUIRED_SYSTEM_PROMPT_PREFIX));
        assert!(system.contains("You are a helpful assistant."));
    }

    #[test]
    fn inject_existing_system_with_prefix_noop() {
        let existing = format!("{REQUIRED_SYSTEM_PROMPT_PREFIX} You are a helpful assistant.");
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": existing,
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_eq!(body["system"].as_str().unwrap(), existing);
    }

    #[test]
    fn inject_haiku_skipped() {
        let mut body = serde_json::json!({
            "model": "claude-haiku-3-20240307",
            "messages": [{"role": "user", "content": "hello"}]
        });
        inject_system_prompt(&mut body);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn inject_haiku_case_insensitive() {
        let mut body = serde_json::json!({
            "model": "claude-3-5-Haiku-20241022",
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn inject_opus_model() {
        let mut body = serde_json::json!({
            "model": "claude-opus-4-20250514",
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_eq!(
            body["system"].as_str().unwrap(),
            REQUIRED_SYSTEM_PROMPT_PREFIX
        );
    }

    #[test]
    fn inject_no_model_field_skipped() {
        let mut body = serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}]
        });
        inject_system_prompt(&mut body);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn inject_haiku_with_existing_system_preserved() {
        let mut body = serde_json::json!({
            "model": "claude-3-haiku-20240307",
            "system": "Custom system prompt",
            "messages": []
        });
        inject_system_prompt(&mut body);
        // Haiku: system field should be untouched
        assert_eq!(body["system"].as_str().unwrap(), "Custom system prompt");
    }
}
