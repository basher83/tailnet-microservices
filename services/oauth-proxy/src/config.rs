//! Configuration types and loading
//!
//! Config precedence: CLI args > env vars > config file > defaults.

use axum::http::{HeaderName, HeaderValue};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Root configuration
#[derive(Debug, Deserialize)]
pub struct Config {
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub headers: Vec<HeaderInjection>,
}

/// HTTP proxy settings
#[derive(Debug, Deserialize)]
pub struct ProxyConfig {
    pub listen_addr: SocketAddr,
    pub upstream_url: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
}

/// Header to inject into proxied requests
#[derive(Debug, Clone, Deserialize)]
pub struct HeaderInjection {
    pub name: String,
    pub value: String,
}

fn default_timeout() -> u64 {
    60
}

fn default_max_connections() -> usize {
    1000
}

impl Config {
    /// Load configuration from a TOML file, then validate.
    pub fn load(path: &Path) -> common::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;

        // Validate upstream_url is a parseable URL with http(s) scheme.
        // Catches malformed URLs at startup rather than on first request.
        let url = reqwest::Url::parse(&config.proxy.upstream_url)
            .map_err(|e| common::Error::Config(format!("upstream_url is not a valid URL: {e}")))?;
        match url.scheme() {
            "http" | "https" => {}
            scheme => {
                return Err(common::Error::Config(format!(
                    "upstream_url must use http or https scheme, got: {scheme}"
                )));
            }
        }

        // Validate timeout_secs is non-zero
        if config.proxy.timeout_secs == 0 {
            return Err(common::Error::Config(
                "timeout_secs must be greater than 0".into(),
            ));
        }

        // Validate max_connections is non-zero
        if config.proxy.max_connections == 0 {
            return Err(common::Error::Config(
                "max_connections must be greater than 0".into(),
            ));
        }

        // Validate header injection entries at load time so misconfigured
        // headers fail fast at startup instead of being silently skipped
        // per-request at runtime.
        for h in &config.headers {
            HeaderName::from_str(&h.name).map_err(|e| {
                common::Error::Config(format!("invalid header name '{}': {e}", h.name))
            })?;
            HeaderValue::from_str(&h.value).map_err(|e| {
                common::Error::Config(format!("invalid header value for '{}': {e}", h.name))
            })?;
        }

        Ok(config)
    }

    /// Resolve config file path from CLI arg or CONFIG_PATH env var.
    pub fn resolve_path(cli_path: Option<&str>) -> PathBuf {
        if let Some(p) = cli_path {
            return PathBuf::from(p);
        }
        if let Ok(p) = std::env::var("CONFIG_PATH") {
            return PathBuf::from(p);
        }
        PathBuf::from("anthropic-oauth-proxy.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mutex to serialize tests that mutate environment variables, preventing
    /// data races when tests run in parallel.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// SAFETY: Callers must hold ENV_MUTEX to prevent concurrent env mutation.
    unsafe fn set_env(key: &str, val: &str) {
        unsafe { std::env::set_var(key, val) };
    }

    unsafe fn remove_env(key: &str) {
        unsafe { std::env::remove_var(key) };
    }

    fn valid_toml() -> &'static str {
        r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"

[[headers]]
name = "anthropic-beta"
value = "oauth-2025-04-20"
"#
    }

    #[test]
    fn test_load_valid_config() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-valid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, valid_toml()).unwrap();

        let config = Config::load(&path).unwrap();
        assert_eq!(config.proxy.upstream_url, "https://api.anthropic.com");
        assert_eq!(config.proxy.timeout_secs, 60);
        assert_eq!(config.proxy.max_connections, 1000);
        assert_eq!(config.headers.len(), 1);
        assert_eq!(config.headers[0].name, "anthropic-beta");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_load_missing_file() {
        let result = Config::load(Path::new("/nonexistent/path/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_invalid_toml() {
        let dir = std::env::temp_dir().join("oauth-proxy-test-invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "not valid {{{{ toml").unwrap();

        let result = Config::load(&path);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_resolve_path_cli_arg() {
        let path = Config::resolve_path(Some("/custom/path.toml"));
        assert_eq!(path, PathBuf::from("/custom/path.toml"));
    }

    #[test]
    fn test_resolve_path_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe { set_env("CONFIG_PATH", "/env/path.toml") };
        let path = Config::resolve_path(None);
        assert_eq!(path, PathBuf::from("/env/path.toml"));
        unsafe { remove_env("CONFIG_PATH") };
    }

    #[test]
    fn test_resolve_path_default() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe { remove_env("CONFIG_PATH") };
        let path = Config::resolve_path(None);
        assert_eq!(path, PathBuf::from("anthropic-oauth-proxy.toml"));
    }

    #[test]
    fn test_resolve_path_cli_overrides_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        unsafe { set_env("CONFIG_PATH", "/env/should-lose.toml") };
        let path = Config::resolve_path(Some("/cli/wins.toml"));
        assert_eq!(
            path,
            PathBuf::from("/cli/wins.toml"),
            "CLI arg must take precedence over CONFIG_PATH env var"
        );
        unsafe { remove_env("CONFIG_PATH") };
    }

    #[test]
    fn test_max_connections_custom() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
max_connections = 500
"#;
        let dir = std::env::temp_dir().join("oauth-proxy-test-maxconn");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, toml_content).unwrap();

        let config = Config::load(&path).unwrap();
        assert_eq!(config.proxy.max_connections, 500);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_invalid_upstream_url_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-bad-url");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "api.anthropic.com"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "upstream_url without scheme must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("upstream_url"),
            "error message should mention upstream_url, got: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_zero_timeout_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-zero-timeout");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
timeout_secs = 0
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err(), "timeout_secs = 0 must be rejected");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_zero_max_connections_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-zero-maxconn");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
max_connections = 0
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err(), "max_connections = 0 must be rejected");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_missing_required_fields_returns_deserialization_error() {
        // Valid TOML syntax but missing required fields should produce a clear
        // deserialization error. This catches the boundary between "invalid TOML
        // syntax" (parser error) and "valid TOML but wrong shape" (serde error).
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-missing-fields");
        std::fs::create_dir_all(&dir).unwrap();

        // Missing [proxy] section entirely
        let toml_no_proxy = r#"
[other]
key = "value"
"#;
        let config_path = dir.join("no_proxy.toml");
        std::fs::write(&config_path, toml_no_proxy).unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "config missing [proxy] section must fail deserialization"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("proxy"),
            "error should mention the missing 'proxy' field, got: {err}"
        );

        // Has [proxy] but missing listen_addr
        let toml_no_addr = r#"
[proxy]
upstream_url = "https://api.anthropic.com"
"#;
        let config_path2 = dir.join("no_addr.toml");
        std::fs::write(&config_path2, toml_no_addr).unwrap();

        let result2 = Config::load(&config_path2);
        assert!(
            result2.is_err(),
            "config missing listen_addr must fail deserialization"
        );
        let err2 = format!("{}", result2.unwrap_err());
        assert!(
            err2.contains("listen_addr"),
            "error should mention the missing 'listen_addr' field, got: {err2}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_unparseable_upstream_url_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-bad-url-parse");
        std::fs::create_dir_all(&dir).unwrap();

        // "https://" alone has no host — reqwest::Url::parse rejects it
        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "upstream_url with no host must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("upstream_url"),
            "error should mention upstream_url, got: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_non_http_scheme_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-ftp-scheme");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "ftp://files.example.com"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err(), "ftp:// scheme must be rejected");
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("http or https"),
            "error should mention required scheme, got: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_invalid_header_name_rejected_at_load() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-bad-header-name");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"

[[headers]]
name = "invalid header name"
value = "fine-value"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "header name with spaces must be rejected at load time"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("invalid header name"),
            "error should identify the bad header name, got: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_invalid_header_value_rejected_at_load() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-bad-header-value");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"

[[headers]]
name = "x-custom"
value = "bad\r\nvalue"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "header value with CRLF must be rejected at load time"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("invalid header value"),
            "error should identify the bad header value, got: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
