// Request-mutation seam regression for INC-2026-001 (stale Claude Code identity).
// Exercises the real `AnthropicOAuthProvider::prepare_request` with an inert
// credential store and a generic-client request shape, then checks the outbound
// identity headers against the last on-wire capture of genuine Claude Code
// (see docs/runbook/header-parity.md) and that every protected mutation still
// holds (credential replacement, body preservation, beta union).
// Origin: Lab Operations INC-2026-001 references/D002/seam.rs (R009), promoted
// into the owner repo on 2026-09-23 (R015).
#[path = "../src/provider_impl.rs"]
mod provider_impl;

use anthropic_auth::{Credential, CredentialStore, REQUIRED_SYSTEM_PROMPT_PREFIX};
use anthropic_pool::Pool;
use provider::Provider;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// Captured 2026-09-23 via `mise run headers:capture` against genuine Claude Code 2.1.280.
const EXPECTED_USER_AGENT: &str = "claude-cli/2.1.280 (external, sdk-cli)";

#[tokio::test]
async fn inc_2026_001_d002() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("D002-fixture.json")).unwrap();
    let mut observations = Vec::new();
    for repetition in 1..=2 {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            CredentialStore::load(directory.path().join("synthetic-only.json"))
                .await
                .unwrap(),
        );
        // Literal placeholders, never real credentials; no refresh path needed.
        store
            .add(
                "synthetic-account".into(),
                Credential {
                    credential_type: "oauth".into(),
                    access: "D002-INERT-NOT-A-TOKEN".into(),
                    refresh: "D002-INERT-NOT-A-REFRESH-TOKEN".into(),
                    expires: u64::MAX,
                },
            )
            .await
            .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let pool = Arc::new(Pool::new(
            vec!["synthetic-account".into()],
            Duration::from_secs(7200),
            store,
            client,
        ));
        let provider = provider_impl::AnthropicOAuthProvider::new(pool);
        let mut headers = HeaderMap::new();
        for (name, value) in fixture["headers"].as_object().unwrap() {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value.as_str().unwrap()).unwrap(),
            );
        }
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer D002-INERT-CLIENT"),
        );
        headers.insert(
            "x-api-key",
            HeaderValue::from_static("D002-INERT-CLIENT-KEY"),
        );
        // A client mirroring the retired proxy constant must not leak it upstream.
        headers.insert(
            "x-anthropic-billing-header",
            HeaderValue::from_static("cc_version=0.0.0.000; cc_entrypoint=test; cch=00000;"),
        );
        let mut body = fixture["json"].clone();
        let result = provider.prepare_request(&mut headers, &mut body).await;
        let prepared = result.is_ok();
        let selected_expected = matches!(result, Ok(Some(ref id)) if id == "synthetic-account");

        // Only allowlisted public identity/compatibility headers are retained.
        // In particular, never serialize HeaderMap or the credential store.
        let names = [
            "user-agent",
            "x-anthropic-billing-header",
            "x-app",
            "anthropic-version",
            "anthropic-beta",
            "anthropic-dangerous-direct-browser-access",
        ];
        let mut identity = BTreeMap::new();
        for name in names {
            identity.insert(name, headers.get(name).and_then(|h| h.to_str().ok()));
        }
        let expected_beta = "oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,claude-code-20250219,prompt-caching-scope-2026-01-05,advanced-tool-use-2025-11-20,extended-cache-ttl-2025-04-11,thinking-token-count-2026-05-13,advisor-tool-2026-03-01,cache-diagnosis-2026-04-07,fine-grained-tool-streaming-2025-05-14";
        let mut expected_body = fixture["json"].clone();
        expected_body["system"] = json!([{"type":"text", "text": REQUIRED_SYSTEM_PROMPT_PREFIX}]);
        let protections = json!({
            "prepared": prepared,
            "selected_synthetic_account": selected_expected,
            "model_unchanged": body["model"] == fixture["json"]["model"],
            "messages_unchanged": body["messages"] == fixture["json"]["messages"],
            "stream_flag_unchanged": body["stream"] == fixture["json"]["stream"],
            "whole_body_only_expected_system_addition": body == expected_body,
            "beta_union_and_dedup_as_frozen": identity["anthropic-beta"] == Some(expected_beta),
            "api_version_preserved_contract": identity["anthropic-version"] == Some("2023-06-01"),
            "app_identity_preserved_contract": identity["x-app"] == Some("cli"),
            "browser_header_preserved_contract": identity["anthropic-dangerous-direct-browser-access"] == Some("true"),
            "client_key_removed": !headers.contains_key("x-api-key"),
            "client_billing_header_removed": !headers.contains_key("x-anthropic-billing-header"),
            "authorization_replaced_with_dummy": headers.get("authorization").is_some_and(|h| h == "Bearer D002-INERT-NOT-A-TOKEN"),
        });
        // INC-2026-001 contract: User-Agent must equal the value captured on
        // the wire from genuine Claude Code 2.1.280 on 2026-09-23 (this is the
        // header Anthropic reads for per-model minimum versions), and the
        // retired x-anthropic-billing-header must not be sent at all (D003/R016:
        // genuine CC sends attribution as a system block, and a present/absent
        // A/B showed the header is not required for acceptance).
        assert_eq!(
            identity["user-agent"],
            Some(EXPECTED_USER_AGENT),
            "user-agent does not match the captured 2.1.280 wire value"
        );
        assert_eq!(
            identity["x-anthropic-billing-header"], None,
            "x-anthropic-billing-header must not be injected"
        );
        let classification = if identity["user-agent"] == Some(EXPECTED_USER_AGENT) {
            "ua-current"
        } else {
            "ua-stale"
        };
        let observation = json!({
            "repetition": repetition,
            "headers": identity,
            "protections": protections,
            "classification": classification,
        });
        println!("D002_OBSERVATION={observation}");
        observations.push(observation);
    }
    // Preserve both sanitized observations before asserting acceptance.
    let repetitions_equal = observations[0]["headers"] == observations[1]["headers"]
        && observations[0]["protections"] == observations[1]["protections"]
        && observations[0]["classification"] == observations[1]["classification"];
    let protections_pass = observations.iter().all(|o| {
        o["protections"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == &json!(true))
    });
    println!(
        "D002_AGGREGATE={}",
        json!({
            "repetitions_equal": repetitions_equal,
            "protections_pass": protections_pass,
            "accepted_local_observation": repetitions_equal && protections_pass,
        })
    );
    assert!(repetitions_equal, "repetition disagreement: inconclusive");
    assert!(
        protections_pass,
        "protection check failed; no positive acceptance"
    );
}
