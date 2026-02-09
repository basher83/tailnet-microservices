//! Anthropic OAuth constants
//!
//! Public OAuth client configuration matching the Claude CLI. These values
//! are not secrets — they identify the public client application. The actual
//! secrets (access/refresh tokens) are managed by the credential store.

/// Anthropic's public OAuth client ID (same as Claude CLI)
pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// OAuth redirect URI (Anthropic's hosted callback page)
pub const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";

/// Token endpoint for code exchange and token refresh
pub const TOKEN_ENDPOINT: &str = "https://console.anthropic.com/v1/oauth/token";

/// Authorization endpoint for Pro/Max subscriptions (claude.ai, not console)
pub const AUTHORIZE_ENDPOINT: &str = "https://claude.ai/oauth/authorize";

/// OAuth scopes required for inference access.
/// `user:sessions:claude_code` is required for Sonnet/Opus access.
/// `org:create_api_key` is deliberately excluded — that's for Console OAuth
/// (API key creation), which is out of scope for this gateway.
pub const SCOPES: &str = "user:profile user:inference user:sessions:claude_code";

/// Required system prompt prefix for Opus/Sonnet access.
/// Anthropic requires this exact string at the start of the system prompt
/// to authorize Claude Code sessions.
pub const REQUIRED_SYSTEM_PROMPT_PREFIX: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";
