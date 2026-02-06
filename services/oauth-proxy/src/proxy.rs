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
use std::time::Duration;
use tracing::{error, instrument, warn};

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
    state
        .requests_total
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let method = request.method().clone();
    let uri = request.uri().clone();

    // Build the upstream URL by appending the request path and query
    let upstream_url = if let Some(pq) = uri.path_and_query() {
        format!("{}{}", state.upstream_url.trim_end_matches('/'), pq)
    } else {
        state.upstream_url.clone()
    };

    // Collect request headers, stripping hop-by-hop
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in request.headers() {
        if !is_hop_by_hop(name.as_str()) {
            headers.insert(name.clone(), value.clone());
        }
    }

    // Inject configured headers (add if not present, replace if present)
    for injection in &state.headers_to_inject {
        let name = match HeaderName::from_str(&injection.name) {
            Ok(n) => n,
            Err(e) => {
                warn!(header = %injection.name, error = %e, "skipping invalid header name");
                continue;
            }
        };
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
    let body_bytes = match axum::body::to_bytes(request.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            state
                .errors_total
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            error!(error = %e, "failed to read request body");
            return error_response(
                StatusCode::BAD_REQUEST,
                &format!("invalid request body: {e}"),
                &request_id,
            );
        }
    };

    // Retry loop: up to 2 retries (3 total attempts) for timeouts only
    let max_attempts = 3u32;
    let retry_delay = Duration::from_millis(100);

    for attempt in 0..max_attempts {
        if attempt > 0 {
            warn!(attempt, "retrying after upstream timeout");
            tokio::time::sleep(retry_delay).await;
        }

        let req = state
            .client
            .request(method.clone(), &upstream_url)
            .headers(headers.clone())
            .timeout(state.timeout)
            .body(body_bytes.clone());

        match req.send().await {
            Ok(upstream_response) => {
                let status = upstream_response.status();
                let resp_headers = upstream_response.headers().clone();

                match upstream_response.bytes().await {
                    Ok(resp_body) => {
                        let mut response = Response::builder().status(status);
                        for (name, value) in &resp_headers {
                            if !is_hop_by_hop(name.as_str()) {
                                response = response.header(name, value);
                            }
                        }
                        return response
                            .body(axum::body::Body::from(resp_body))
                            .unwrap_or_else(|e| {
                                error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    &format!("response build error: {e}"),
                                    &request_id,
                                )
                            });
                    }
                    Err(e) => {
                        state
                            .errors_total
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        error!(error = %e, "failed to read upstream response body");
                        return error_response(
                            StatusCode::BAD_GATEWAY,
                            &format!("upstream response read error: {e}"),
                            &request_id,
                        );
                    }
                }
            }
            Err(e) if e.is_timeout() && attempt < max_attempts - 1 => {
                // Timeout and we have retries left — continue loop
                continue;
            }
            Err(e) if e.is_timeout() => {
                state
                    .errors_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                error!(error = %e, attempts = max_attempts, "upstream timeout after all retries");
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    &format!(
                        "upstream timeout after {}s ({max_attempts} attempts)",
                        state.timeout.as_secs()
                    ),
                    &request_id,
                );
            }
            Err(e) => {
                state
                    .errors_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                error!(error = %e, "upstream request failed");
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("upstream error: {e}"),
                    &request_id,
                );
            }
        }
    }

    // Should be unreachable, but handle defensively
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected retry exhaustion",
        &request_id,
    )
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
    fn test_hop_by_hop_detection() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("TRANSFER-ENCODING"));
        assert!(is_hop_by_hop("keep-alive"));
        assert!(is_hop_by_hop("Proxy-Authorization"));
        assert!(!is_hop_by_hop("Content-Type"));
        assert!(!is_hop_by_hop("Authorization"));
        assert!(!is_hop_by_hop("X-Custom-Header"));
    }

    #[test]
    fn test_error_response_format() {
        let resp = error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream timeout after 60s",
            "req_abc123",
        );
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    }
}
