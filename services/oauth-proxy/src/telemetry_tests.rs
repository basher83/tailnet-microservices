//! Tests for telemetry span recording. Included into `telemetry.rs` via
//! `#[path]` to keep each file within the structural LOC limit.

use super::*;
use crate::params::ProxyParams;

#[test]
fn init_tracer_returns_none_when_env_unset() {
    // Ensure the env var is not set for this test.
    // SAFETY: test runs in a single-threaded context; no other thread
    // reads this env var concurrently during this test.
    unsafe {
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    let result: Option<(
        SdkTracerProvider,
        OpenTelemetryLayer<tracing_subscriber::Registry, _>,
    )> = init_tracer();
    assert!(
        result.is_none(),
        "init_tracer must return None when OTEL_EXPORTER_OTLP_ENDPOINT is unset"
    );
}

#[test]
fn metadata_json_format_correct() {
    let metadata = build_metadata(
        Some("acct_123"),
        None,
        0,
        "req_abc",
        "oauth",
        &ParamsFixture::none(),
    );
    assert_eq!(metadata["proxy.account_id"], "acct_123");
    assert!(metadata["proxy.error_type"].is_null());
    assert_eq!(metadata["proxy.failover_attempt"], 0);
    assert_eq!(metadata["proxy.request_id"], "req_abc");
    assert_eq!(metadata["proxy.pool_mode"], "oauth");
}

#[test]
fn metadata_json_passthrough_mode() {
    let metadata = build_metadata(
        None,
        None,
        0,
        "req_xyz",
        "passthrough",
        &ParamsFixture::none(),
    );
    assert!(metadata["proxy.account_id"].is_null());
    assert_eq!(metadata["proxy.pool_mode"], "passthrough");
}

#[test]
fn metadata_json_error_with_failover() {
    let metadata = build_metadata(
        Some("acct_456"),
        Some("quota_exhausted"),
        3,
        "req_fail",
        "oauth",
        &ParamsFixture::none(),
    );
    assert_eq!(metadata["proxy.account_id"], "acct_456");
    assert_eq!(metadata["proxy.error_type"], "quota_exhausted");
    assert_eq!(metadata["proxy.failover_attempt"], 3);
}

#[test]
fn build_metadata_merges_q25_params_as_native_types() {
    // V3 at the emission boundary: the real merged metadata dict carries the
    // Q25 proxy.* fields as native JSON int/bool/float (non-stringified), so a
    // downstream filter like metadata.proxy.max_tokens > 8000 matches.
    let params = ParamsFixture::param_rich();
    let metadata = build_metadata(Some("acct"), None, 0, "req", "oauth", &params);
    assert!(metadata["proxy.max_tokens"].is_i64());
    assert_eq!(metadata["proxy.max_tokens"], 32000);
    assert!(metadata["proxy.stream"].is_boolean());
    assert!(metadata["proxy.temperature"].is_f64());
    assert_eq!(metadata["proxy.thinking_enabled"], true);
    assert_eq!(metadata["proxy.tools_count"], 5);
    // Base routing fields still coexist with the new params.
    assert_eq!(metadata["proxy.pool_mode"], "oauth");
    // Round-trips through the on-wire string form Phoenix load_json_strings sees.
    let reparsed: serde_json::Value = serde_json::from_str(&metadata.to_string()).unwrap();
    assert!(reparsed["proxy.max_tokens"].is_i64());
}

#[test]
fn record_span_does_not_panic_without_otel_layer() {
    // When no OTel layer is installed, record_span should be a safe no-op.
    record_span(
        "POST",
        "/v1/messages",
        "api.anthropic.com",
        200,
        Some("acct_test"),
        None,
        0,
        "req_test",
        "oauth",
        &ProxyParams::default(),
    );
}

#[test]
fn record_span_error_status_codes() {
    // Verify various status codes don't panic.
    for status in [200, 301, 400, 401, 429, 500, 503] {
        let et = if status >= 400 {
            Some("transient")
        } else {
            None
        };
        record_span(
            "GET",
            "/v1/messages",
            "api.anthropic.com",
            status,
            None,
            et,
            0,
            "req_status",
            "passthrough",
            &ProxyParams::default(),
        );
    }
}

/// Small fixture builder so telemetry tests stay independent of params internals.
struct ParamsFixture;
impl ParamsFixture {
    fn none() -> ProxyParams {
        ProxyParams::default()
    }
    fn param_rich() -> ProxyParams {
        ProxyParams::extract(Some(&serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 32000,
            "temperature": 0.7,
            "stream": true,
            "thinking": {"type": "enabled", "budget_tokens": 8000},
            "tools": [{}, {}, {}, {}, {}]
        })))
    }
}
