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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_includes_context() {
        let config_err = Error::Config("missing field".into());
        assert_eq!(config_err.to_string(), "Configuration error: missing field");

        let io_err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        assert!(
            io_err.to_string().starts_with("I/O error:"),
            "got: {}",
            io_err
        );
    }

    #[test]
    fn error_debug_includes_variant() {
        let err = Error::Config("bad value".into());
        let debug = format!("{:?}", err);
        assert!(
            debug.contains("Config"),
            "Debug should include variant name, got: {debug}"
        );
    }

    #[test]
    fn toml_parse_error_display_includes_context() {
        let bad_toml = "not { valid toml @@";
        let toml_err: std::result::Result<toml::Value, _> = toml::from_str(bad_toml);
        let err = Error::Toml(toml_err.unwrap_err());
        let display = err.to_string();
        assert!(
            display.starts_with("TOML parse error:"),
            "Toml error display must start with 'TOML parse error:', got: {display}"
        );
    }

    #[test]
    fn toml_error_debug_includes_variant() {
        let bad_toml = "{{invalid";
        let toml_err: std::result::Result<toml::Value, _> = toml::from_str(bad_toml);
        let err = Error::Toml(toml_err.unwrap_err());
        let debug = format!("{:?}", err);
        assert!(
            debug.contains("Toml"),
            "Debug should include Toml variant name, got: {debug}"
        );
    }
}
