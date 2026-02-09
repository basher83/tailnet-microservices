//! OAuth token exchange and refresh
//!
//! Handles the two token endpoint interactions:
//! 1. Authorization code exchange (initial OAuth flow completion)
//! 2. Token refresh (proactive and request-time refresh)
//!
//! Both operations POST to `TOKEN_ENDPOINT` with different grant types.
//! The token endpoint is Anthropic's console (`console.anthropic.com`),
//! not the inference API (`api.anthropic.com`).

use serde::{Deserialize, Serialize};

use crate::constants::{ANTHROPIC_CLIENT_ID, REDIRECT_URI, TOKEN_ENDPOINT};
use crate::error::{Error, Result};

/// Response from the token endpoint for both exchange and refresh.
///
/// `expires_in` is a delta in seconds from the response time. The caller
/// converts this to an absolute unix millisecond timestamp when storing
/// the credential.
#[derive(Debug, Deserialize, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds until the access token expires (delta, not absolute)
    pub expires_in: u64,
}

/// Exchange an authorization code for tokens (initial OAuth flow).
///
/// This is the second step of the PKCE flow: the user has authorized
/// in their browser, and we received the authorization code. We send
/// the code along with the PKCE verifier to prove we initiated the flow.
pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse> {
    let response = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("client_id", ANTHROPIC_CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await
        .map_err(|e| Error::Http(format!("token exchange request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<no body>"));
        return Err(Error::TokenExchange(format!(
            "token endpoint returned {status}: {body}"
        )));
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(|e| Error::TokenExchange(format!("invalid token response: {e}")))
}

/// Refresh an access token using a refresh token.
///
/// Called proactively by the background refresh task (before expiration)
/// and reactively at request time (when token is about to expire).
pub async fn refresh_token(client: &reqwest::Client, refresh: &str) -> Result<TokenResponse> {
    let response = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", ANTHROPIC_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| Error::Http(format!("token refresh request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<no body>"));

        // 401/403 means the refresh token is revoked or invalid
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(Error::InvalidCredentials(format!(
                "refresh token rejected ({status}): {body}"
            )));
        }

        return Err(Error::TokenExchange(format!(
            "token refresh returned {status}: {body}"
        )));
    }

    response
        .json::<TokenResponse>()
        .await
        .map_err(|e| Error::TokenExchange(format!("invalid refresh response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_response_deserializes() {
        let json = r#"{"access_token":"at_abc","refresh_token":"rt_def","expires_in":3600}"#;
        let token: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(token.access_token, "at_abc");
        assert_eq!(token.refresh_token, "rt_def");
        assert_eq!(token.expires_in, 3600);
    }

    #[test]
    fn token_response_serializes() {
        let token = TokenResponse {
            access_token: "at_test".into(),
            refresh_token: "rt_test".into(),
            expires_in: 3600,
        };
        let json = serde_json::to_string(&token).unwrap();
        assert!(json.contains("\"access_token\":\"at_test\""));
        assert!(json.contains("\"refresh_token\":\"rt_test\""));
        assert!(json.contains("\"expires_in\":3600"));
    }

    #[test]
    fn exchange_uses_correct_endpoint() {
        assert_eq!(
            TOKEN_ENDPOINT,
            "https://console.anthropic.com/v1/oauth/token"
        );
    }

    #[test]
    fn exchange_includes_client_id() {
        // Verify the client ID constant is the known Anthropic public OAuth client
        assert_eq!(ANTHROPIC_CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    #[test]
    fn exchange_includes_redirect_uri() {
        assert_eq!(
            REDIRECT_URI,
            "https://console.anthropic.com/oauth/code/callback"
        );
    }

    #[tokio::test]
    async fn exchange_code_rejects_invalid_code() {
        // Sending a bogus authorization code to the real token endpoint
        // returns a non-success error (400 or similar)
        let client = reqwest::Client::new();
        let result = exchange_code(&client, "invalid-code", "invalid-verifier").await;
        assert!(result.is_err(), "invalid code must return error");
    }

    #[tokio::test]
    async fn refresh_token_rejects_invalid_token() {
        // Sending a bogus refresh token returns a non-success error
        let client = reqwest::Client::new();
        let result = refresh_token(&client, "rt_invalid").await;
        assert!(result.is_err(), "invalid refresh token must return error");
    }
}
