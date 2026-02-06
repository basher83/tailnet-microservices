//! Secret wrapper for sensitive values

use std::fmt;
use zeroize::Zeroize;

/// Sensitive value - redacted in Debug/Display/logs
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    /// Create a new secret value
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Expose the inner value (use sparingly)
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T: Zeroize> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T: Zeroize> Drop for Secret<T> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<T: Zeroize + Clone> Clone for Secret<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_redacts_debug() {
        let secret = Secret::new(String::from("my-api-key"));
        let debug = format!("{:?}", secret);
        assert_eq!(debug, "[REDACTED]");
        assert!(!debug.contains("my-api-key"));
    }

    #[test]
    fn test_secret_exposes_value() {
        let secret = Secret::new(String::from("my-api-key"));
        assert_eq!(secret.expose(), "my-api-key");
    }
}
