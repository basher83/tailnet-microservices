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

    #[test]
    fn test_secret_redacts_display() {
        let secret = Secret::new(String::from("super-secret-token"));
        let display = format!("{}", secret);
        assert_eq!(display, "[REDACTED]");
        assert!(!display.contains("super-secret-token"));
    }

    #[test]
    fn test_secret_clone_preserves_value() {
        let secret = Secret::new(String::from("clone-me"));
        let cloned = secret.clone();
        assert_eq!(cloned.expose(), "clone-me");
        // Both the original and clone must independently expose the value
        assert_eq!(secret.expose(), cloned.expose());
    }

    #[test]
    fn test_secret_clone_is_independent() {
        let secret = Secret::new(String::from("independent"));
        let cloned = secret.clone();
        // Dropping the original must not affect the clone
        drop(secret);
        assert_eq!(cloned.expose(), "independent");
    }

    #[test]
    fn test_secret_zeroizes_on_drop() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        /// Tracks whether zeroize() was called via a shared flag.
        #[derive(Clone)]
        struct Witness {
            zeroed: Arc<AtomicBool>,
        }

        impl Zeroize for Witness {
            fn zeroize(&mut self) {
                self.zeroed.store(true, Ordering::SeqCst);
            }
        }

        let zeroed = Arc::new(AtomicBool::new(false));
        let secret = Secret::new(Witness {
            zeroed: Arc::clone(&zeroed),
        });

        assert!(
            !zeroed.load(Ordering::SeqCst),
            "must not zeroize before drop"
        );
        drop(secret);
        assert!(zeroed.load(Ordering::SeqCst), "must zeroize on drop");
    }
}
