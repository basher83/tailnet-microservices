//! Anthropic OAuth authentication library
//!
//! Provides PKCE flow generation, token exchange/refresh, and credential
//! file storage for the Anthropic OAuth gateway. This crate is a standalone
//! library with no dependency on the proxy binary — it can be tested and
//! used independently.
//!
//! Credential flow:
//! 1. Admin calls `pkce::generate_verifier()` + `pkce::compute_challenge()`
//! 2. User authorizes via `pkce::build_authorization_url()`
//! 3. Gateway calls `token::exchange_code()` with the authorization code
//! 4. Credential stored via `credentials::CredentialStore::add()`
//! 5. Background task calls `token::refresh_token()` proactively
//! 6. Updated tokens saved via `credentials::CredentialStore::update_token()`

pub mod constants;
pub mod credentials;
pub mod error;
pub mod pkce;
pub mod token;

pub use constants::*;
pub use credentials::{Credential, CredentialStore};
pub use error::{Error, Result};
pub use pkce::{build_authorization_url, compute_challenge, generate_verifier};
pub use token::{TokenResponse, exchange_code, refresh_token};
