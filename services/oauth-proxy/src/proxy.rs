//! HTTP proxy logic
//!
//! Receives inbound requests, strips hop-by-hop headers, delegates auth to the
//! provider, and forwards to the upstream URL. Returns the upstream response
//! verbatim (including error status codes from upstream).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use provider::Provider;
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
    pub provider: Arc<dyn Provider>,
    pub timeout: Duration,
    pub requests_total: Arc<std::sync::atomic::AtomicU64>,
    pub errors_total: Arc<std::sync::atomic::AtomicU64>,
    pub in_flight: Arc<std::sync::atomic::AtomicU64>,
    /// Maximum failover attempts for quota exhaustion. Equals pool size in OAuth
    /// mode (each attempt uses a different account). Set to 1 in passthrough mode
    /// (no failover, just forward the error).
    pub max_failover_attempts: usize,
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

/// Proxy an inbound request to upstream with header injection, retries, and failover.
///
/// Retry strategy per spec: UpstreamTimeout gets 2 retries with 100ms fixed backoff.
/// Failover strategy: QuotaExceeded triggers account switch and re-send; Permanent
/// errors disable the account and return the error; Transient errors are returned.
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

    // Collect original request headers, stripping hop-by-hop, host, and
    // content-length. Host carries the proxy's hostname — reqwest sets the
    // correct one from the upstream URL. Content-Length must be recalculated
    // by reqwest/hyper because OAuth mode re-serializes the body (system
    // prompt injection changes byte count). Forwarding the client's original
    // Content-Length causes a mismatch that Cloudflare rejects with 400.
    //
    // Uses append() instead of insert() to preserve multi-value headers
    // (e.g. multiple Cookie or Accept-Encoding values from the client).
    let mut original_headers = reqwest::header::HeaderMap::new();
    for (name, value) in request.headers() {
        if !is_hop_by_hop(name.as_str())
            && name != axum::http::header::HOST
            && name != axum::http::header::CONTENT_LENGTH
        {
            original_headers.append(name.clone(), value.clone());
        }
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

    // Parse body JSON once if the provider needs it (OAuth mode needs body for
    // system prompt injection). The parsed value is re-used across failover attempts.
    let parsed_body = if state.provider.needs_body() {
        match serde_json::from_slice::<serde_json::Value>(&body_bytes) {
            Ok(v) => Some(v),
            Err(e) => {
                state
                    .errors_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let status = StatusCode::BAD_REQUEST;
                crate::metrics::record_request(
                    status.as_u16(),
                    &method_str,
                    start.elapsed().as_secs_f64(),
                );
                crate::metrics::record_upstream_error("invalid_request");
                return error_response(status, &format!("Invalid JSON body: {e}"), &request_id);
            }
        }
    } else {
        None
    };

    // Maximum failover attempts equals the pool size (each attempt uses a
    // different account). Passthrough mode uses 1 attempt (no failover).
    let max_failovers = state.max_failover_attempts;

    for failover in 0..max_failovers {
        // Start from original headers each attempt so provider injection is clean.
        // Without this, headers from a previous failed account (e.g. wrong Bearer
        // token) would carry over into the next attempt.
        let mut headers = original_headers.clone();
        let mut body_value = parsed_body.clone().unwrap_or(serde_json::Value::Null);

        let account_id = match state
            .provider
            .prepare_request(&mut headers, &mut body_value)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                state
                    .errors_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let status = StatusCode::SERVICE_UNAVAILABLE;
                crate::metrics::record_request(
                    status.as_u16(),
                    &method_str,
                    start.elapsed().as_secs_f64(),
                );
                error!(error = %e, "provider prepare_request failed");
                return error_response(status, &format!("provider error: {e}"), &request_id);
            }
        };

        let final_body = if state.provider.needs_body() {
            serde_json::to_vec(&body_value)
                .unwrap_or_else(|_| body_bytes.to_vec())
                .into()
        } else {
            body_bytes.clone()
        };

        // Timeout retry loop within this failover attempt
        let mut last_error_response = None;

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
                .body(final_body.clone());

            match req.send().await {
                Ok(upstream_response) => {
                    let status = upstream_response.status();

                    // For error responses that may need classification (quota/auth
                    // errors), buffer the body. For success or non-classifiable
                    // errors, stream directly.
                    if status.is_client_error() || status.is_server_error() {
                        if let Some(ref acct) = account_id {
                            // Buffer error body for classification
                            let resp_headers = upstream_response.headers().clone();
                            let error_body = upstream_response.bytes().await.unwrap_or_default();
                            let error_body_str = String::from_utf8_lossy(&error_body).to_string();

                            let classification = state
                                .provider
                                .classify_error(status.as_u16(), &error_body_str);

                            match classification {
                                provider::ErrorClassification::QuotaExceeded => {
                                    warn!(
                                        account_id = acct,
                                        failover, "quota exhausted, failing over to next account"
                                    );
                                    let _ = state.provider.report_error(acct, classification).await;
                                    crate::metrics::record_upstream_error("quota_exhausted");
                                    crate::metrics::record_pool_quota_exhaustion(acct);
                                    crate::metrics::record_pool_failover(acct, "quota_exhausted");
                                    crate::metrics::record_pool_account_status(
                                        acct,
                                        "cooling_down",
                                    );
                                    // Store response in case this is the last failover
                                    last_error_response = Some((status, resp_headers, error_body));
                                    break; // exit timeout retry loop, continue failover loop
                                }
                                provider::ErrorClassification::Permanent => {
                                    warn!(account_id = acct, "permanent error, disabling account");
                                    let _ = state.provider.report_error(acct, classification).await;
                                    crate::metrics::record_upstream_error("permanent");
                                    crate::metrics::record_pool_account_status(acct, "disabled");
                                    // Return error to client immediately
                                    let elapsed = start.elapsed();
                                    crate::metrics::record_request(
                                        status.as_u16(),
                                        &method_str,
                                        elapsed.as_secs_f64(),
                                    );
                                    state
                                        .errors_total
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    return build_buffered_response(
                                        status,
                                        &resp_headers,
                                        error_body,
                                    );
                                }
                                provider::ErrorClassification::Transient => {
                                    // Return error to client (existing timeout retry
                                    // handles transport-level retries)
                                    let elapsed = start.elapsed();
                                    crate::metrics::record_request(
                                        status.as_u16(),
                                        &method_str,
                                        elapsed.as_secs_f64(),
                                    );
                                    info!(
                                        status = status.as_u16(),
                                        latency_ms = elapsed.as_millis() as u64,
                                        "request completed (transient error)"
                                    );
                                    return build_buffered_response(
                                        status,
                                        &resp_headers,
                                        error_body,
                                    );
                                }
                            }
                        } else {
                            // Passthrough mode: no account, stream error response directly
                            let resp_headers = upstream_response.headers().clone();
                            let elapsed = start.elapsed();
                            crate::metrics::record_request(
                                status.as_u16(),
                                &method_str,
                                elapsed.as_secs_f64(),
                            );
                            info!(
                                status = status.as_u16(),
                                latency_ms = elapsed.as_millis() as u64,
                                "request completed"
                            );
                            return build_streaming_response(
                                status,
                                &resp_headers,
                                upstream_response,
                                &request_id,
                            );
                        }
                    }

                    // Success: stream the response body. This is critical for SSE
                    // (Server-Sent Events) from the Anthropic API where Claude
                    // responses are streamed in real-time.
                    let resp_headers = upstream_response.headers().clone();
                    let elapsed = start.elapsed();
                    crate::metrics::record_request(
                        status.as_u16(),
                        &method_str,
                        elapsed.as_secs_f64(),
                    );
                    info!(
                        status = status.as_u16(),
                        latency_ms = elapsed.as_millis() as u64,
                        "request completed"
                    );
                    return build_streaming_response(
                        status,
                        &resp_headers,
                        upstream_response,
                        &request_id,
                    );
                }
                Err(e) if e.is_timeout() && attempt < MAX_UPSTREAM_ATTEMPTS - 1 => {
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
                    error!(
                        error = %e,
                        attempts = MAX_UPSTREAM_ATTEMPTS,
                        "upstream timeout after all retries"
                    );
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
                    return error_response(
                        err_status,
                        &format!("upstream error: {e}"),
                        &request_id,
                    );
                }
            }
        }

        // If we broke out of the timeout loop due to quota exhaustion but have
        // more failover attempts, continue to the next account
        if last_error_response.is_some() && failover < max_failovers - 1 {
            continue;
        }

        // Last failover attempt exhausted — return the last error response
        if let Some((status, resp_headers, error_body)) = last_error_response {
            state
                .errors_total
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::metrics::record_request(
                status.as_u16(),
                &method_str,
                start.elapsed().as_secs_f64(),
            );
            return build_buffered_response(status, &resp_headers, error_body);
        }
    }

    unreachable!("failover loop must return on every code path")
}

/// Build a response from a buffered error body (used after error classification).
fn build_buffered_response(
    status: StatusCode,
    resp_headers: &reqwest::header::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let mut response = Response::builder().status(status);
    for (name, value) in resp_headers {
        if !is_hop_by_hop(name.as_str()) {
            response = response.header(name, value);
        }
    }
    response
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("response build error: {e}"),
                "",
            )
        })
}

/// Build a streaming response (used for success and passthrough error responses).
fn build_streaming_response(
    status: StatusCode,
    resp_headers: &reqwest::header::HeaderMap,
    upstream_response: reqwest::Response,
    request_id: &str,
) -> Response {
    let mut response = Response::builder().status(status);
    for (name, value) in resp_headers {
        if !is_hop_by_hop(name.as_str()) {
            response = response.header(name, value);
        }
    }
    response
        .body(axum::body::Body::from_stream(
            upstream_response.bytes_stream(),
        ))
        .unwrap_or_else(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("response build error: {e}"),
                request_id,
            )
        })
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
