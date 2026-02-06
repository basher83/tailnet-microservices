//! HTTP proxy logic

use crate::config::HeaderInjection;

/// Headers to strip before forwarding (hop-by-hop)
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

/// Proxy a request to upstream with header injection
pub async fn proxy_request(
    _request: axum::http::Request<axum::body::Body>,
    _upstream_url: &str,
    _headers_to_inject: &[HeaderInjection],
) -> crate::error::Result<axum::response::Response> {
    // TODO: Implement proxy logic
    // 1. Clone request
    // 2. Strip hop-by-hop headers
    // 3. Inject configured headers
    // 4. Forward to upstream
    // 5. Return response
    todo!("Implement proxy logic")
}

/// Check if header is hop-by-hop
pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP_HEADERS.iter().any(|h| h.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hop_by_hop_detection() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("TRANSFER-ENCODING"));
        assert!(!is_hop_by_hop("Content-Type"));
        assert!(!is_hop_by_hop("Authorization"));
    }
}
