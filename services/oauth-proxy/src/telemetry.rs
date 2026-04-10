//! OpenTelemetry trace instrumentation (D6 API-boundary layer)
//!
//! Controlled by `OTEL_EXPORTER_OTLP_ENDPOINT`. When unset, no OTel SDK is
//! initialized — zero overhead, identical to pre-D6 behavior. When set, each
//! proxied request produces one OTLP span exported via gRPC to the configured
//! endpoint (typically Phoenix on the same cluster).
//!
//! Custom proxy attributes are emitted as a single `metadata` JSON string
//! attribute (Phoenix `load_json_strings` deserializes it for DSL queries).

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::registry::LookupSpan;

/// Initialize the OTel TracerProvider if `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
///
/// Returns `Some((provider, layer))` when tracing is enabled:
/// - `provider` must be retained and shut down during graceful shutdown
/// - `layer` is added to the tracing subscriber registry
///
/// The OTel SDK reads `OTEL_EXPORTER_OTLP_ENDPOINT` automatically for the
/// exporter endpoint, so the env var serves as both toggle and configuration.
pub fn init_tracer<S>() -> Option<(
    SdkTracerProvider,
    OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>,
)>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .expect("failed to create OTLP span exporter");

    let resource = Resource::builder()
        .with_service_name("anthropic-oauth-proxy")
        .with_attributes([
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("collector.source", "talos-cluster"),
        ])
        .build();

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer("anthropic-oauth-proxy");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Some((provider, layer))
}

/// Record OTel span attributes on the current tracing span.
///
/// Sets top-level OTel semantic convention attributes and a `metadata` JSON
/// dict attribute containing proxy-specific fields. Safe to call when OTel is
/// disabled — all `set_attribute` calls are no-ops without an OTel layer.
#[allow(clippy::too_many_arguments)]
pub fn record_span(
    method: &str,
    path: &str,
    server_address: &str,
    status_code: u16,
    account_id: Option<&str>,
    error_type: Option<&str>,
    failover_attempt: usize,
    request_id: &str,
    pool_mode: &str,
) {
    use opentelemetry::trace::Status;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let span = tracing::Span::current();

    // Top-level OTel semantic convention attributes
    span.set_attribute("http.request.method", method.to_string());
    span.set_attribute("http.response.status_code", status_code as i64);
    span.set_attribute("url.path", path.to_string());
    span.set_attribute("server.address", server_address.to_string());

    // OTel span status derived from HTTP status
    if status_code >= 400 {
        span.set_status(Status::error(""));
    } else {
        span.set_status(Status::Ok);
    }

    // Metadata JSON dict — Phoenix deserializes via load_json_strings,
    // making keys queryable as metadata["proxy.account_id"] etc.
    let metadata = serde_json::json!({
        "proxy.account_id": account_id,
        "proxy.error_type": error_type,
        "proxy.failover_attempt": failover_attempt,
        "proxy.request_id": request_id,
        "proxy.pool_mode": pool_mode,
    });
    span.set_attribute("metadata", metadata.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let metadata = serde_json::json!({
            "proxy.account_id": Some("acct_123"),
            "proxy.error_type": None::<String>,
            "proxy.failover_attempt": 0,
            "proxy.request_id": "req_abc",
            "proxy.pool_mode": "oauth",
        });
        let json_str = metadata.to_string();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed["proxy.account_id"], "acct_123");
        assert!(parsed["proxy.error_type"].is_null());
        assert_eq!(parsed["proxy.failover_attempt"], 0);
        assert_eq!(parsed["proxy.request_id"], "req_abc");
        assert_eq!(parsed["proxy.pool_mode"], "oauth");
    }

    #[test]
    fn metadata_json_passthrough_mode() {
        let metadata = serde_json::json!({
            "proxy.account_id": None::<String>,
            "proxy.error_type": None::<String>,
            "proxy.failover_attempt": 0,
            "proxy.request_id": "req_xyz",
            "proxy.pool_mode": "passthrough",
        });
        let parsed: serde_json::Value = serde_json::from_str(&metadata.to_string()).unwrap();

        assert!(parsed["proxy.account_id"].is_null());
        assert_eq!(parsed["proxy.pool_mode"], "passthrough");
    }

    #[test]
    fn metadata_json_error_with_failover() {
        let metadata = serde_json::json!({
            "proxy.account_id": Some("acct_456"),
            "proxy.error_type": Some("quota_exhausted"),
            "proxy.failover_attempt": 3,
            "proxy.request_id": "req_fail",
            "proxy.pool_mode": "oauth",
        });
        let parsed: serde_json::Value = serde_json::from_str(&metadata.to_string()).unwrap();

        assert_eq!(parsed["proxy.account_id"], "acct_456");
        assert_eq!(parsed["proxy.error_type"], "quota_exhausted");
        assert_eq!(parsed["proxy.failover_attempt"], 3);
    }

    #[test]
    fn record_span_does_not_panic_without_otel_layer() {
        // When no OTel layer is installed, record_span should be a safe no-op
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
        );
    }

    #[test]
    fn record_span_error_status_codes() {
        // Verify various status codes don't panic
        for status in [200, 301, 400, 401, 429, 500, 503] {
            record_span(
                "GET",
                "/v1/messages",
                "api.anthropic.com",
                status,
                None,
                if status >= 400 {
                    Some("transient")
                } else {
                    None
                },
                0,
                "req_status",
                "passthrough",
            );
        }
    }
}
