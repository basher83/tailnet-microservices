//! Quota detection for Anthropic API responses
//!
//! Distinguishes between transient rate limits (429 with generic message) and
//! subscription quota exhaustion (429 with 5-hour rolling window message).
//! Only quota exhaustion triggers account cooldown and failover.

use provider::ErrorClassification;

/// Quota exhaustion message patterns in Anthropic 429 responses.
///
/// These indicate the account has hit the 5-hour rolling subscription limit,
/// not a transient per-minute rate limit.
const QUOTA_PATTERNS: &[&str] = &[
    "5-hour",
    "5 hour",
    "rolling window",
    "usage limit for your plan",
    "subscription usage limit",
];

/// Classify a 429 response body as quota exhaustion or transient rate limit.
///
/// Checks the response body for known quota exhaustion phrases. If any match,
/// returns `QuotaExceeded` (account should enter cooldown). Otherwise returns
/// `Transient` (normal rate limit, retry on same account).
pub fn classify_429(body: &str) -> ErrorClassification {
    let lower = body.to_lowercase();
    for pattern in QUOTA_PATTERNS {
        if lower.contains(pattern) {
            return ErrorClassification::QuotaExceeded;
        }
    }
    ErrorClassification::Transient
}

/// Classify an upstream error by HTTP status and response body.
///
/// Dispatches to `classify_429` for 429 responses. Other statuses use fixed
/// classification: 401/403 are Permanent (invalid credentials), 408/5xx are
/// Transient (retryable), everything else is Transient.
pub fn classify_status(status: u16, body: &str) -> ErrorClassification {
    match status {
        429 => classify_429(body),
        401 | 403 => ErrorClassification::Permanent,
        408 | 500 | 502 | 503 | 504 => ErrorClassification::Transient,
        _ => ErrorClassification::Transient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_429_five_hour_dash() {
        let body = r#"{"error":{"message":"You've exceeded your 5-hour usage limit"}}"#;
        assert_eq!(classify_429(body), ErrorClassification::QuotaExceeded);
    }

    #[test]
    fn classify_429_five_hour_space() {
        let body = r#"{"error":{"message":"Exceeded 5 hour rolling limit"}}"#;
        assert_eq!(classify_429(body), ErrorClassification::QuotaExceeded);
    }

    #[test]
    fn classify_429_rolling_window() {
        let body = r#"{"error":{"message":"Rate limited by rolling window quota"}}"#;
        assert_eq!(classify_429(body), ErrorClassification::QuotaExceeded);
    }

    #[test]
    fn classify_429_usage_limit_for_plan() {
        let body = r#"{"error":{"message":"You have reached the usage limit for your plan"}}"#;
        assert_eq!(classify_429(body), ErrorClassification::QuotaExceeded);
    }

    #[test]
    fn classify_429_subscription_usage_limit() {
        let body = r#"{"error":{"message":"subscription usage limit exceeded"}}"#;
        assert_eq!(classify_429(body), ErrorClassification::QuotaExceeded);
    }

    #[test]
    fn classify_429_non_matching_is_transient() {
        let body = r#"{"error":{"message":"Rate limit exceeded, please retry"}}"#;
        assert_eq!(classify_429(body), ErrorClassification::Transient);
    }

    #[test]
    fn classify_429_empty_body_is_transient() {
        assert_eq!(classify_429(""), ErrorClassification::Transient);
    }

    #[test]
    fn classify_429_case_insensitive() {
        let body = r#"{"error":{"message":"5-HOUR USAGE LIMIT EXCEEDED"}}"#;
        assert_eq!(classify_429(body), ErrorClassification::QuotaExceeded);
    }

    #[test]
    fn classify_status_429_delegates() {
        let body = r#"{"error":{"message":"5-hour limit hit"}}"#;
        assert_eq!(
            classify_status(429, body),
            ErrorClassification::QuotaExceeded
        );
    }

    #[test]
    fn classify_status_401_permanent() {
        assert_eq!(
            classify_status(401, "unauthorized"),
            ErrorClassification::Permanent
        );
    }

    #[test]
    fn classify_status_403_permanent() {
        assert_eq!(
            classify_status(403, "forbidden"),
            ErrorClassification::Permanent
        );
    }

    #[test]
    fn classify_status_500_transient() {
        assert_eq!(
            classify_status(500, "internal server error"),
            ErrorClassification::Transient
        );
    }

    #[test]
    fn classify_status_502_transient() {
        assert_eq!(
            classify_status(502, "bad gateway"),
            ErrorClassification::Transient
        );
    }

    #[test]
    fn classify_status_503_transient() {
        assert_eq!(
            classify_status(503, "service unavailable"),
            ErrorClassification::Transient
        );
    }

    #[test]
    fn classify_status_504_transient() {
        assert_eq!(
            classify_status(504, "gateway timeout"),
            ErrorClassification::Transient
        );
    }

    #[test]
    fn classify_status_408_transient() {
        assert_eq!(
            classify_status(408, "request timeout"),
            ErrorClassification::Transient
        );
    }

    #[test]
    fn classify_status_unknown_is_transient() {
        assert_eq!(
            classify_status(418, "i'm a teapot"),
            ErrorClassification::Transient
        );
    }
}
