//! HTTP proxy logic
//!
//! Receives inbound requests, strips hop-by-hop headers, injects configured
//! headers, and forwards to the upstream URL. Returns the upstream response
//! verbatim (including error status codes from upstream).

use crate::config::HeaderInjection;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, instrument, warn};

/// Maximum retry attempts for upstream timeouts (spec: 2 retries = 3 total attempts)
const MAX_UPSTREAM_ATTEMPTS: u32 = 3;

/// Fixed backoff between upstream timeout retries (spec: 100ms)
const UPSTREAM_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Maximum request body size (spec: 10 MiB)
pub const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Headers to strip before forwarding (hop-by-hop per RFC 2616 Section 13.5.1)
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Shared state passed to the proxy handler via axum State extractor
#[derive(Clone)]
pub struct ProxyState {
    pub client: reqwest::Client,
    pub upstream_url: String,
    pub headers_to_inject: Vec<HeaderInjection>,
    pub timeout: Duration,
    pub requests_total: Arc<std::sync::atomic::AtomicU64>,
    pub errors_total: Arc<std::sync::atomic::AtomicU64>,
    pub in_flight: Arc<std::sync::atomic::AtomicU64>,
}

/// RAII guard that decrements the in-flight counter when dropped, ensuring the
/// counter stays accurate even if the handler returns early or panics.
struct InFlightGuard(Arc<std::sync::atomic::AtomicU64>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// JSON error response per spec: {"error":{"type":"proxy_error","message":"...","request_id":"req_..."}}
fn error_response(status: StatusCode, message: &str, request_id: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "type": "proxy_error",
            "message": message,
            "request_id": request_id,
        }
    });
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// Proxy an inbound request to upstream with header injection and retries.
///
/// Retry strategy per spec: UpstreamTimeout gets 2 retries with 100ms fixed backoff.
#[instrument(skip_all, fields(request_id = %request_id, method = %request.method(), path = %request.uri().path()))]
pub async fn proxy_request(
    state: &ProxyState,
    request: axum::http::Request<axum::body::Body>,
    request_id: String,
) -> Response {
    let start = Instant::now();
    state
        .requests_total
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state
        .in_flight
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _in_flight_guard = InFlightGuard(state.in_flight.clone());

    let method = request.method().clone();
    let method_str = method.to_string();
    let uri = request.uri().clone();

    // Build the upstream URL by appending the request path and query
    let upstream_url = if let Some(pq) = uri.path_and_query() {
        format!("{}{}", state.upstream_url.trim_end_matches('/'), pq)
    } else {
        state.upstream_url.clone()
    };

    // Collect request headers, stripping hop-by-hop and host.
    // The host header carries the proxy's hostname (e.g. "anthropic-oauth-proxy")
    // but the upstream expects its own hostname (e.g. "api.anthropic.com").
    // Reqwest automatically sets the correct Host from the upstream URL.
    //
    // Uses append() instead of insert() to preserve multi-value headers
    // (e.g. multiple Cookie or Accept-Encoding values from the client).
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in request.headers() {
        if !is_hop_by_hop(name.as_str()) && name != axum::http::header::HOST {
            headers.append(name.clone(), value.clone());
        }
    }

    // Inject configured headers (add if not present, replace if present).
    // The authorization header is protected per spec: it must always pass
    // through from the client unchanged, regardless of injection config.
    for injection in &state.headers_to_inject {
        let name = match HeaderName::from_str(&injection.name) {
            Ok(n) => n,
            Err(e) => {
                warn!(header = %injection.name, error = %e, "skipping invalid header name");
                continue;
            }
        };
        if name == axum::http::header::AUTHORIZATION {
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

    // Read the request body
    let body_bytes = match axum::body::to_bytes(request.into_body(), MAX_BODY_SIZE).await {
        Ok(b) => b,
        Err(e) => {
            state
                .errors_total
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            error!(error = %e, "failed to read request body");
            let status = StatusCode::BAD_REQUEST;
            crate::metrics::record_request(
                status.as_u16(),
                &method_str,
                start.elapsed().as_secs_f64(),
            );
            crate::metrics::record_upstream_error("invalid_request");
            return error_response(status, &format!("invalid request body: {e}"), &request_id);
        }
    };

    // Retry loop: up to 2 retries (3 total attempts) for timeouts only
    for attempt in 0..MAX_UPSTREAM_ATTEMPTS {
        if attempt > 0 {
            warn!(attempt, "retrying after upstream timeout");
            tokio::time::sleep(UPSTREAM_RETRY_DELAY).await;
        }

        let req = state
            .client
            .request(method.clone(), &upstream_url)
            .headers(headers.clone())
            .timeout(state.timeout)
            .body(body_bytes.clone());

        match req.send().await {
            Ok(upstream_response) => {
                // Collect status and headers before consuming the body stream.
                // This allows metrics recording even for streamed responses (SSE).
                let status = upstream_response.status();
                let resp_headers = upstream_response.headers().clone();
                let elapsed = start.elapsed();

                crate::metrics::record_request(status.as_u16(), &method_str, elapsed.as_secs_f64());
                info!(
                    status = status.as_u16(),
                    latency_ms = elapsed.as_millis() as u64,
                    "request completed"
                );

                // Stream the response body instead of buffering it. This is
                // critical for SSE (Server-Sent Events) from the Anthropic API
                // where Claude responses are streamed in real-time. Buffering
                // would break streaming semantics and use unbounded memory.
                let mut response = Response::builder().status(status);
                for (name, value) in &resp_headers {
                    if !is_hop_by_hop(name.as_str()) {
                        response = response.header(name, value);
                    }
                }
                return response
                    .body(axum::body::Body::from_stream(
                        upstream_response.bytes_stream(),
                    ))
                    .unwrap_or_else(|e| {
                        error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &format!("response build error: {e}"),
                            &request_id,
                        )
                    });
            }
            Err(e) if e.is_timeout() && attempt < MAX_UPSTREAM_ATTEMPTS - 1 => {
                // Timeout and we have retries left — continue loop
                continue;
            }
            Err(e) if e.is_timeout() => {
                state
                    .errors_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let err_status = StatusCode::GATEWAY_TIMEOUT;
                crate::metrics::record_request(
                    err_status.as_u16(),
                    &method_str,
                    start.elapsed().as_secs_f64(),
                );
                crate::metrics::record_upstream_error("timeout");
                error!(error = %e, attempts = MAX_UPSTREAM_ATTEMPTS, "upstream timeout after all retries");
                return error_response(
                    err_status,
                    &format!(
                        "upstream timeout after {}s ({MAX_UPSTREAM_ATTEMPTS} attempts)",
                        state.timeout.as_secs()
                    ),
                    &request_id,
                );
            }
            Err(e) => {
                state
                    .errors_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let err_status = StatusCode::BAD_GATEWAY;
                crate::metrics::record_request(
                    err_status.as_u16(),
                    &method_str,
                    start.elapsed().as_secs_f64(),
                );
                crate::metrics::record_upstream_error("connection");
                error!(error = %e, "upstream request failed");
                return error_response(err_status, &format!("upstream error: {e}"), &request_id);
            }
        }
    }

    // Should be unreachable, but handle defensively
    let err_status = StatusCode::INTERNAL_SERVER_ERROR;
    crate::metrics::record_request(
        err_status.as_u16(),
        &method_str,
        start.elapsed().as_secs_f64(),
    );
    crate::metrics::record_upstream_error("internal");
    error_response(err_status, "unexpected retry exhaustion", &request_id)
}

/// Check if a header is hop-by-hop (should be stripped before forwarding)
pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP_HEADERS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hop_by_hop_detection_all_eight_headers() {
        // All 8 spec-defined hop-by-hop headers (RFC 2616 Section 13.5.1)
        assert!(is_hop_by_hop("connection"));
        assert!(is_hop_by_hop("keep-alive"));
        assert!(is_hop_by_hop("proxy-authenticate"));
        assert!(is_hop_by_hop("proxy-authorization"));
        assert!(is_hop_by_hop("te"));
        assert!(is_hop_by_hop("trailer"));
        assert!(is_hop_by_hop("transfer-encoding"));
        assert!(is_hop_by_hop("upgrade"));

        // Case-insensitive detection (all 8 headers)
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("TRANSFER-ENCODING"));
        assert!(is_hop_by_hop("Keep-Alive"));
        assert!(is_hop_by_hop("Proxy-Authenticate"));
        assert!(is_hop_by_hop("Proxy-Authorization"));
        assert!(is_hop_by_hop("TE"));
        assert!(is_hop_by_hop("Trailer"));
        assert!(is_hop_by_hop("Upgrade"));

        // Non-hop-by-hop headers must not match
        assert!(!is_hop_by_hop("Content-Type"));
        assert!(!is_hop_by_hop("Authorization"));
        assert!(!is_hop_by_hop("X-Custom-Header"));
        assert!(!is_hop_by_hop("Accept-Encoding"));
    }

    #[tokio::test]
    async fn test_error_response_format() {
        let resp = error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream timeout after 60s",
            "req_abc123",
        );
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

        // Verify the response body matches the spec JSON format:
        // {"error":{"type":"proxy_error","message":"...","request_id":"..."}}
        let body = resp.into_body();
        let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["type"], "proxy_error");
        assert_eq!(json["error"]["message"], "upstream timeout after 60s");
        assert_eq!(json["error"]["request_id"], "req_abc123");
    }

    #[tokio::test]
    async fn test_error_response_content_type() {
        let resp = error_response(StatusCode::BAD_GATEWAY, "upstream error", "req_test123");
        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            content_type, "application/json",
            "error responses must have application/json Content-Type"
        );
    }

    #[test]
    fn test_in_flight_guard_decrements_on_drop() {
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);

        {
            let _guard = InFlightGuard(counter.clone());
        }
        // Guard dropped, counter should be decremented
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn test_in_flight_guard_multiple_concurrent() {
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));

        // Simulate 3 in-flight requests
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _g1 = InFlightGuard(counter.clone());
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _g2 = InFlightGuard(counter.clone());
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _g3 = InFlightGuard(counter.clone());
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 3);

        drop(_g1);
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 2);

        drop(_g2);
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);

        drop(_g3);
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
