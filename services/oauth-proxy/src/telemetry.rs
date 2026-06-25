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
    params: &crate::params::ProxyParams,
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
    let metadata = build_metadata(
        account_id,
        error_type,
        failover_attempt,
        request_id,
        pool_mode,
        params,
    );
    span.set_attribute("metadata", metadata.to_string());
}

/// Build the `metadata` JSON dict attached to the span. Pure and testable: the
/// base routing fields plus Q25's request-parameter fields, merged in as
/// native-typed JSON values (numbers/bools stay non-stringified for Phoenix).
fn build_metadata(
    account_id: Option<&str>,
    error_type: Option<&str>,
    failover_attempt: usize,
    request_id: &str,
    pool_mode: &str,
    params: &crate::params::ProxyParams,
) -> serde_json::Value {
    let mut metadata = serde_json::json!({
        "proxy.account_id": account_id,
        "proxy.error_type": error_type,
        "proxy.failover_attempt": failover_attempt,
        "proxy.request_id": request_id,
        "proxy.pool_mode": pool_mode,
    });
    if let Some(map) = metadata.as_object_mut() {
        params.write_into(map);
    }
    metadata
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
