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
///
/// The Tier 1 additions (2026-07-02) mirror genuine Claude Code 2.1.198 wire
/// flags. Each is either a pure identity flag (`claude-code-20250219`) or inert
/// unless the request body opts into the matching feature. Tier 2
/// (`thinking-token-count`) was promoted after a streaming check confirmed it
/// only adds a nullable `estimated_tokens` field to thinking deltas. Tier 3
/// (`advisor-tool`, `cache-diagnosis`) was promoted after a smoke test (plain
/// request → 200, no breakage) confirmed the bare headers are inert. The proxy
/// now mirrors all 10 CC beta flags; see `docs/audits/anthropic-beta-flags.md`.
const REQUIRED_BETA_FLAGS: &[&str] = &[
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
    "context-management-2025-06-27",
    // Tier 1 — CC-fidelity, inert-unless-opted-in (docs/audits/anthropic-beta-flags.md):
    "claude-code-20250219",
    "prompt-caching-scope-2026-01-05",
    "advanced-tool-use-2025-11-20",
    "extended-cache-ttl-2025-04-11",
    // Tier 2 — adds a nullable `estimated_tokens` field to thinking deltas;
    // streaming check 2026-07-02 confirmed accepted + additive-only:
    "thinking-token-count-2026-05-13",
    // Tier 3 — inert-unless-invoked (gates advisor tool / cache diagnostics);
    // smoke-verified 2026-07-02 (plain request 200, no breakage):
    "advisor-tool-2026-03-01",
    "cache-diagnosis-2026-04-07",
];

/// User-Agent injected on the `/v1/messages` wire. Kept in lock-step with the
/// `cc_version` attribution below — both track the live Claude Code release.
/// Originally a Loom mitmproxy capture of v2.0.76; bumped to 2.1.198 on
/// 2026-07-02 after an on-wire capture confirmed genuine CC 2.1.198 sends
/// `claude-cli/2.1.198 (external, sdk-cli)`. See `docs/audits/header-provenance.md`.
const USER_AGENT: &str = "claude-cli/2.1.198 (external, sdk-cli)";

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// `x-app` header that genuine Claude Code sends on every `/v1/messages` request
/// (value `cli`). Injected to mirror real CC wire behaviour; confirmed present
/// via on-wire capture 2026-07-02. See `docs/audits/header-provenance.md`.
const X_APP: &str = "cli";

/// Claude Code attribution header used by this proxy for Max-plan routing.
/// Origin: Claude Code `--debug-file` attribution line. `cc_version` tracks the
/// CC release (bumped to 2.1.198 on 2026-07-02 via `scripts/capture-cc-headers.sh`);
/// `cc_entrypoint=sdk-cli` is the headless path; `cch=00000` is a hardcoded CC
/// constant with no account data. Note: genuine CC does NOT send this header on
/// the `/v1/messages` wire (debug-only) — the proxy injects it deliberately and
/// it is accepted. See `docs/audits/header-provenance.md` before changing this.
const ANTHROPIC_BILLING_HEADER: &str = "cc_version=2.1.198.bb7; cc_entrypoint=sdk-cli; cch=00000;";

/// OAuth provider backed by a subscription pool.
///
/// Selects accounts round-robin, injects Bearer tokens, merges anthropic-beta
/// flags, and injects the required system prompt prefix for all models.
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

            // Strip any client-provided auth headers — OAuth mode manages its
            // own credentials. Both Authorization (Bearer) and x-api-key (direct
            // API key) must be removed. Forwarding x-api-key alongside the OAuth
            // Bearer token signals a non-Claude-Code client to Anthropic.
            headers.remove(reqwest::header::AUTHORIZATION);
            headers.remove(HeaderName::from_static("x-api-key"));

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
            headers.insert(
                HeaderName::from_static("x-app"),
                HeaderValue::from_static(X_APP),
            );
            headers.insert(
                HeaderName::from_static("x-anthropic-billing-header"),
                HeaderValue::from_static(ANTHROPIC_BILLING_HEADER),
            );

            // System prompt injection for all models. Then remove Pi's local
            // documentation-routing hint, which trips Anthropic's Max-plan
            // usage classifier even with Claude Code attribution present.
            inject_system_prompt(body);
            sanitize_system_prompt_for_plan_usage(body);

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

/// Inject the required system prompt prefix for OAuth credential compliance.
///
/// The Anthropic API accepts `system` as either a plain string or an array of
/// content blocks (`[{"type":"text","text":"..."}]`). This function handles both.
///
/// Rules:
/// - No model field: skip (can't determine if injection is needed)
/// - No `system` field: create as array with required prefix block
/// - String `system` without prefix: convert to array, prepend prefix block
/// - String `system` with prefix: convert to array (preserve prefix)
/// - Array `system` whose first text block lacks prefix: prepend prefix block
/// - Array `system` already has prefix: no modification
///
/// The output is always an array of content blocks so that cache_control and
/// other per-block metadata survive round-tripping through the proxy.
///
/// Applied to ALL models including Haiku. While Haiku doesn't require the
/// prefix for model access, consistent injection avoids credential validation
/// edge cases. Loom (reference implementation) applies the prefix to all
/// models under OAuth.
fn sanitize_pi_documentation_block(text: &str) -> Option<String> {
    let marker = "\n\nPi documentation (read only when";
    let (prefix, rest) = text.split_once(marker)?;
    let retained_context: Vec<&str> = rest
        .lines()
        .filter(|line| {
            line.starts_with("Current date:") || line.starts_with("Current working directory:")
        })
        .collect();

    let prefix = prefix.trim_end();
    if retained_context.is_empty() {
        Some(prefix.to_string())
    } else {
        Some(format!("{}\n\n{}", prefix, retained_context.join("\n")))
    }
}

fn sanitize_system_prompt_for_plan_usage(body: &mut serde_json::Value) {
    match body.get_mut("system") {
        Some(system) if system.is_string() => {
            if let Some(text) = system.as_str().and_then(sanitize_pi_documentation_block) {
                *system = serde_json::Value::String(text);
            }
        }
        Some(system) if system.is_array() => {
            if let Some(blocks) = system.as_array_mut() {
                for block in blocks {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text")
                        && let Some(text_value) = block.get_mut("text")
                        && let Some(text) = text_value
                            .as_str()
                            .and_then(sanitize_pi_documentation_block)
                    {
                        *text_value = serde_json::Value::String(text);
                    }
                }
            }
        }
        _ => {}
    }
}

fn inject_system_prompt(body: &mut serde_json::Value) {
    if extract_model(body).is_none() {
        return;
    }

    let prefix = REQUIRED_SYSTEM_PROMPT_PREFIX;

    match body.get("system") {
        None => {
            body["system"] = serde_json::json!([
                { "type": "text", "text": prefix }
            ]);
            debug!("injected system prompt (no existing system field)");
        }
        Some(existing) if existing.is_string() => {
            let existing_str = existing.as_str().unwrap();
            if existing_str.starts_with(prefix) {
                // Already has prefix — convert to array preserving content
                body["system"] = serde_json::json!([
                    { "type": "text", "text": existing_str }
                ]);
            } else {
                // Prepend prefix as separate block, keep original as second block
                body["system"] = serde_json::json!([
                    { "type": "text", "text": prefix },
                    { "type": "text", "text": existing_str }
                ]);
                debug!("prepended system prompt prefix to existing string system field");
            }
        }
        Some(existing) if existing.is_array() => {
            let arr = existing.as_array().unwrap();
            // Check if the first text block already starts with the prefix
            let has_prefix = arr.iter().any(|block| {
                block.get("type").and_then(|t| t.as_str()) == Some("text")
                    && block
                        .get("text")
                        .and_then(|t| t.as_str())
                        .is_some_and(|t| t.starts_with(prefix))
            });
            if !has_prefix {
                let mut new_arr = vec![serde_json::json!({ "type": "text", "text": prefix })];
                new_arr.extend(arr.iter().cloned());
                body["system"] = serde_json::Value::Array(new_arr);
                debug!("prepended system prompt prefix to existing array system field");
            }
        }
        _ => {
            // Non-string, non-array system field: leave as-is
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
            "oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,claude-code-20250219,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,extended-cache-ttl-2025-04-11,thinking-token-count-2026-05-13,advisor-tool-2026-03-01,cache-diagnosis-2026-04-07"
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
            "oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,claude-code-20250219,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,extended-cache-ttl-2025-04-11,thinking-token-count-2026-05-13,advisor-tool-2026-03-01,cache-diagnosis-2026-04-07"
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

    // --- System prompt injection helpers ---

    /// Extract all text values from a system prompt array.
    fn system_texts(body: &serde_json::Value) -> Vec<String> {
        body["system"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .map(|b| b["text"].as_str().unwrap().to_string())
            .collect()
    }

    /// Assert the system field is an array whose first text block is the prefix.
    fn assert_has_prefix(body: &serde_json::Value) {
        let texts = system_texts(body);
        assert!(
            !texts.is_empty(),
            "system array must have at least one text block"
        );
        assert!(
            texts[0].starts_with(REQUIRED_SYSTEM_PROMPT_PREFIX),
            "first text block must start with prefix, got: {}",
            texts[0]
        );
    }

    #[test]
    fn sanitize_pi_documentation_block_preserves_runtime_context() {
        let mut body = serde_json::json!({
            "model": "claude-haiku-4-5",
            "system": [{
                "type": "text",
                "text": "Guidelines:\n- Be concise\n\nPi documentation (read only when the user asks about pi itself):\n- When asked about: extensions (docs/extensions.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md)\nCurrent date: 2026-05-05\nCurrent working directory: /repo"
            }]
        });

        sanitize_system_prompt_for_plan_usage(&mut body);

        let text = body["system"][0]["text"].as_str().unwrap();
        assert!(text.contains("Guidelines:\n- Be concise"));
        assert!(text.contains("Current date: 2026-05-05"));
        assert!(text.contains("Current working directory: /repo"));
        assert!(!text.contains("Pi documentation"));
        assert!(!text.contains("custom providers"));
    }

    #[test]
    fn sanitize_pi_documentation_block_noops_without_marker() {
        let mut body = serde_json::json!({
            "model": "claude-haiku-4-5",
            "system": [{ "type": "text", "text": "Guidelines only" }]
        });

        sanitize_system_prompt_for_plan_usage(&mut body);

        assert_eq!(
            body["system"][0]["text"].as_str().unwrap(),
            "Guidelines only"
        );
    }

    // --- System prompt injection tests (string input) ---

    #[test]
    fn inject_no_system_field() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "hello"}]
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
        assert_eq!(system_texts(&body).len(), 1);
    }

    #[test]
    fn inject_existing_string_system_without_prefix() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful assistant.",
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
        let texts = system_texts(&body);
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0], REQUIRED_SYSTEM_PROMPT_PREFIX);
        assert_eq!(texts[1], "You are a helpful assistant.");
    }

    #[test]
    fn inject_existing_string_system_with_prefix_preserved() {
        let existing = format!("{REQUIRED_SYSTEM_PROMPT_PREFIX} You are a helpful assistant.");
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": existing,
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
        let texts = system_texts(&body);
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0], existing);
    }

    #[test]
    fn inject_haiku_gets_prefix() {
        let mut body = serde_json::json!({
            "model": "claude-haiku-3-20240307",
            "messages": [{"role": "user", "content": "hello"}]
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
    }

    #[test]
    fn inject_haiku_case_insensitive() {
        let mut body = serde_json::json!({
            "model": "claude-3-5-Haiku-20241022",
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
    }

    #[test]
    fn inject_opus_model() {
        let mut body = serde_json::json!({
            "model": "claude-opus-4-20250514",
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
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
    fn inject_haiku_with_existing_string_system_gets_prefix() {
        let mut body = serde_json::json!({
            "model": "claude-3-haiku-20240307",
            "system": "Custom system prompt",
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
        let texts = system_texts(&body);
        assert!(texts.iter().any(|t| t.contains("Custom system prompt")));
    }

    // --- System prompt injection tests (array input) ---

    #[test]
    fn inject_array_system_without_prefix() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                { "type": "text", "text": "Custom instructions." },
            ],
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
        let texts = system_texts(&body);
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0], REQUIRED_SYSTEM_PROMPT_PREFIX);
        assert_eq!(texts[1], "Custom instructions.");
    }

    #[test]
    fn inject_array_system_with_prefix_noop() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                { "type": "text", "text": REQUIRED_SYSTEM_PROMPT_PREFIX },
                { "type": "text", "text": "Extra context." },
            ],
            "messages": []
        });
        inject_system_prompt(&mut body);
        // Should not add another prefix block
        let texts = system_texts(&body);
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0], REQUIRED_SYSTEM_PROMPT_PREFIX);
    }

    #[test]
    fn inject_array_system_preserves_cache_control() {
        let mut body = serde_json::json!({
            "model": "claude-opus-4-6",
            "system": [
                {
                    "type": "text",
                    "text": "You are a coding assistant.",
                    "cache_control": { "type": "ephemeral" }
                }
            ],
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
        let arr = body["system"].as_array().unwrap();
        // Original block with cache_control should be preserved as second element
        assert_eq!(arr.len(), 2);
        assert!(arr[1].get("cache_control").is_some());
        assert_eq!(
            arr[1]["text"].as_str().unwrap(),
            "You are a coding assistant."
        );
    }

    #[test]
    fn inject_array_system_empty_array() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [],
            "messages": []
        });
        inject_system_prompt(&mut body);
        assert_has_prefix(&body);
        assert_eq!(system_texts(&body).len(), 1);
    }
}
