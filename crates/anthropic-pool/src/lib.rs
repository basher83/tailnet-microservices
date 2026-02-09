//! Subscription pool for Anthropic OAuth accounts
//!
//! Manages multiple Claude Max subscription accounts with round-robin selection,
//! quota detection, cooldown state machine, and proactive token refresh. The pool
//! reads credentials from `CredentialStore` (single source of truth) and maintains
//! per-account status independently.
//!
//! Account lifecycle:
//! 1. Admin adds account via admin API → credential stored, status `Available`
//! 2. Pool selects account round-robin → check/refresh token, return access token
//! 3. Upstream returns 429 with quota message → `CoolingDown` for cooldown duration
//! 4. Upstream returns 401/403 → `Disabled` permanently
//! 5. Cooldown expires → automatic transition back to `Available`
//! 6. Background task refreshes tokens proactively before expiration

pub mod error;
pub mod pool;
pub mod quota;
pub mod refresh;

pub use error::{Error, Result};
pub use pool::{AccountStatus, Pool, SelectedAccount};
pub use quota::{classify_429, classify_status};
pub use refresh::spawn_refresh_task;
