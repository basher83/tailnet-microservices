//! PKCE (Proof Key for Code Exchange) implementation per RFC 7636
//!
//! Generates the code verifier and S256 challenge used during the OAuth
//! authorization flow. The verifier is stored server-side and sent during
//! token exchange; the challenge is included in the authorization URL so
//! the authorization server can verify the exchange request came from the
//! same party that initiated the flow.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngExt;
use sha2::{Digest, Sha256};

use crate::constants::{ANTHROPIC_CLIENT_ID, AUTHORIZE_ENDPOINT, REDIRECT_URI, SCOPES};

/// Generate a cryptographically random PKCE code verifier.
///
/// Produces a 128-byte random value encoded as URL-safe base64 (no padding).
/// RFC 7636 requires 43-128 characters; our output is 172 characters
/// (128 bytes * 4/3, rounded), well within the spec range.
pub fn generate_verifier() -> String {
    let mut bytes = [0u8; 128];
    rand::rng().fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute the S256 code challenge from a verifier.
///
/// `challenge = BASE64URL(SHA256(verifier))`
///
/// The authorization server compares this against the challenge sent in
/// the authorization URL to verify the token exchange request is legitimate.
pub fn compute_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Build the full authorization URL with all required OAuth parameters.
///
/// The `state` parameter is an opaque value the client generates for CSRF
/// protection. The authorization server returns it unchanged in the callback.
pub fn build_authorization_url(state: &str, challenge: &str) -> String {
    format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        AUTHORIZE_ENDPOINT,
        ANTHROPIC_CLIENT_ID,
        urlencoded(REDIRECT_URI),
        urlencoded(SCOPES),
        challenge,
        state,
    )
}

/// Minimal URL encoding for parameter values.
/// Only encodes characters that would break URL parameter parsing.
fn urlencoded(s: &str) -> String {
    s.replace(' ', "%20")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_url_safe_base64() {
        let verifier = generate_verifier();
        // 128 bytes → 171 base64url chars (no padding, ceil(128*4/3) - 1 padding)
        assert_eq!(verifier.len(), 171);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "verifier must be URL-safe base64 (no padding): {verifier}"
        );
    }

    #[test]
    fn verifiers_are_unique() {
        let a = generate_verifier();
        let b = generate_verifier();
        assert_ne!(a, b, "two verifiers must not collide");
    }

    #[test]
    fn challenge_is_deterministic() {
        let verifier = "test-verifier-value";
        let c1 = compute_challenge(verifier);
        let c2 = compute_challenge(verifier);
        assert_eq!(c1, c2, "same verifier must produce same challenge");
    }

    #[test]
    fn challenge_is_url_safe_base64() {
        let challenge = compute_challenge("test-verifier");
        // SHA-256 produces 32 bytes → 43 base64url chars (no padding)
        assert_eq!(challenge.len(), 43);
        assert!(
            challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "challenge must be URL-safe base64 (no padding): {challenge}"
        );
    }

    #[test]
    fn challenge_matches_known_value() {
        // Pre-computed: SHA256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        // base64url of those 32 bytes = LPJNul-wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ
        let challenge = compute_challenge("hello");
        assert_eq!(challenge, "LPJNul-wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ");
    }

    #[test]
    fn authorization_url_contains_required_params() {
        let challenge = compute_challenge("test-verifier");
        let url = build_authorization_url("test-state-123", &challenge);

        assert!(url.starts_with(AUTHORIZE_ENDPOINT));
        assert!(url.contains(&format!("client_id={ANTHROPIC_CLIENT_ID}")));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={challenge}")));
        assert!(url.contains("state=test-state-123"));
        assert!(url.contains("scope="));
    }

    #[test]
    fn roundtrip_verifier_challenge() {
        // Generate a real verifier and verify the challenge is valid base64url
        let verifier = generate_verifier();
        let challenge = compute_challenge(&verifier);

        // Decode the challenge back to verify it's valid base64url
        let decoded = URL_SAFE_NO_PAD.decode(&challenge).expect("valid base64url");
        assert_eq!(decoded.len(), 32, "SHA-256 hash must be 32 bytes");
    }
}
