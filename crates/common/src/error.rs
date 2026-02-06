//! Common error types

use thiserror::Error;

/// Common error type
#[derive(Error, Debug)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Result alias using common Error
pub type Result<T> = std::result::Result<T, Error>;
