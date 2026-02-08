//! Prometheus metrics exposition
//!
//! Registers and exposes the metrics defined in specs/oauth-proxy.md:
//!
//! - `proxy_requests_total` (counter): labels `status`, `method`
//! - `proxy_request_duration_seconds` (histogram): label `status`
//! - `proxy_upstream_errors_total` (counter): label `error_type`

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Install the Prometheus recorder and return a handle for rendering metrics.
///
/// Configures `proxy_request_duration_seconds` with histogram buckets so it
/// renders as a Prometheus histogram (with `_bucket` lines for `histogram_quantile()`
/// queries) rather than the default summary. Bucket boundaries cover the range
/// from 5ms to 60s, matching the proxy's configurable timeout range.
///
/// The handle's `render()` method produces the Prometheus text exposition format
/// suitable for serving on a `/metrics` endpoint.
pub fn install_recorder() -> PrometheusHandle {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(
                "proxy_request_duration_seconds".to_string(),
            ),
            &[
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
            ],
        )
        .expect("failed to set histogram buckets")
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

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_exporter_prometheus::PrometheusRecorder;

    #[test]
    fn record_functions_do_not_panic_without_recorder() {
        // When no recorder is installed, metrics calls are no-ops.
        // This verifies the functions don't panic in test environments.
        record_request(200, "GET", 0.05);
        record_upstream_error("timeout");
    }

    /// Create an isolated recorder/handle pair for unit tests.
    /// Uses build_recorder() instead of install_recorder() to avoid the
    /// global recorder singleton constraint — only one global recorder can
    /// exist per process, and install_recorder() panics on a second call.
    fn isolated_recorder() -> (PrometheusRecorder, PrometheusHandle) {
        let recorder = PrometheusBuilder::new()
            .set_buckets_for_metric(
                metrics_exporter_prometheus::Matcher::Full(
                    "proxy_request_duration_seconds".to_string(),
                ),
                &[
                    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
                ],
            )
            .expect("failed to set histogram buckets")
            .build_recorder();
        let handle = recorder.handle();
        (recorder, handle)
    }

    #[test]
    fn record_request_increments_counter_and_histogram() {
        // Verifies that record_request() actually writes to the Prometheus
        // recorder so that /metrics renders the expected counter and histogram
        // lines. Without an installed recorder these calls are silent no-ops,
        // which would leave operators with empty dashboards.
        let (recorder, handle) = isolated_recorder();
        let _guard = metrics::set_default_local_recorder(&recorder);

        record_request(200, "GET", 0.042);
        record_request(500, "POST", 1.5);

        let output = handle.render();
        assert!(
            output.contains("proxy_requests_total"),
            "rendered output must contain proxy_requests_total counter"
        );
        assert!(
            output.contains("status=\"200\""),
            "counter must carry status label"
        );
        assert!(
            output.contains("method=\"GET\""),
            "counter must carry method label"
        );
        assert!(
            output.contains("status=\"500\""),
            "second request status label must appear"
        );
        assert!(
            output.contains("method=\"POST\""),
            "second request method label must appear"
        );
        assert!(
            output.contains("proxy_request_duration_seconds_bucket"),
            "histogram must render _bucket lines for histogram_quantile() queries"
        );
    }

    #[test]
    fn record_upstream_error_increments_counter_with_label() {
        // Verifies the upstream error counter is recorded with the error_type
        // label so that operators can alert on specific failure modes (timeout
        // vs connection refused vs other).
        let (recorder, handle) = isolated_recorder();
        let _guard = metrics::set_default_local_recorder(&recorder);

        record_upstream_error("timeout");
        record_upstream_error("connection");

        let output = handle.render();
        assert!(
            output.contains("proxy_upstream_errors_total"),
            "rendered output must contain proxy_upstream_errors_total counter"
        );
        assert!(
            output.contains("error_type=\"timeout\""),
            "error_type label must be recorded"
        );
        assert!(
            output.contains("error_type=\"connection\""),
            "distinct error_type values must appear separately"
        );
    }

    #[test]
    fn histogram_buckets_cover_spec_range() {
        // The spec requires histogram buckets from 5ms to 60s so that
        // histogram_quantile() queries in Grafana/RUNBOOK produce meaningful
        // results. Without explicit buckets, metrics-exporter-prometheus
        // renders summaries (quantiles) instead of histograms (_bucket lines),
        // breaking all RUNBOOK PromQL.
        let (recorder, handle) = isolated_recorder();
        let _guard = metrics::set_default_local_recorder(&recorder);

        record_request(200, "GET", 0.003); // 3ms, below lowest bucket

        let output = handle.render();
        // Verify specific bucket boundaries from the spec
        assert!(output.contains("le=\"0.005\""), "5ms bucket must exist");
        assert!(output.contains("le=\"0.01\""), "10ms bucket must exist");
        assert!(
            output.contains("le=\"60\""),
            "60s bucket must exist (upper bound of timeout range)"
        );
        assert!(
            output.contains("le=\"+Inf\""),
            "+Inf bucket must exist (Prometheus convention)"
        );
    }
}
