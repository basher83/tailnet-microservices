//! Prometheus metrics exposition
//!
//! Registers and exposes the metrics defined in specs/oauth-proxy.md:
//!
//! - `proxy_requests_total` (counter): labels `status`, `method`
//! - `proxy_request_duration_seconds` (histogram): label `status`
//! - `proxy_upstream_errors_total` (counter): label `error_type`
//! - `tailnet_connected` (gauge): 1 when connected, 0 otherwise

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Install the Prometheus recorder and return a handle for rendering metrics.
///
/// The handle's `render()` method produces the Prometheus text exposition format
/// suitable for serving on a `/metrics` endpoint.
pub fn install_recorder() -> PrometheusHandle {
    PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}

/// Record a completed proxy request with status code and HTTP method labels.
pub fn record_request(status: u16, method: &str, duration_secs: f64) {
    let status_str = status.to_string();
    metrics::counter!("proxy_requests_total", "status" => status_str.clone(), "method" => method.to_string())
        .increment(1);
    metrics::histogram!("proxy_request_duration_seconds", "status" => status_str)
        .record(duration_secs);
}

/// Record an upstream error with a classification label.
pub fn record_upstream_error(error_type: &str) {
    metrics::counter!("proxy_upstream_errors_total", "error_type" => error_type.to_string())
        .increment(1);
}

/// Set the tailnet connection gauge (1 = connected, 0 = disconnected).
pub fn set_tailnet_connected(connected: bool) {
    metrics::gauge!("tailnet_connected").set(if connected { 1.0 } else { 0.0 });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_functions_do_not_panic_without_recorder() {
        // When no recorder is installed, metrics calls are no-ops.
        // This verifies the functions don't panic in test environments.
        record_request(200, "GET", 0.05);
        record_upstream_error("timeout");
        set_tailnet_connected(true);
        set_tailnet_connected(false);
    }
}
