//! Configuration types and loading
//!
//! Config precedence: CLI args > env vars > config file > defaults.
//! The auth_key is loaded from TS_AUTHKEY env var or auth_key_file,
//! never stored in the TOML directly to avoid leaking secrets.

use common::Secret;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Root configuration
#[derive(Debug, Deserialize)]
pub struct Config {
    pub tailscale: TailscaleConfig,
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub headers: Vec<HeaderInjection>,
}

/// Tailnet connection settings
#[derive(Debug, Deserialize)]
pub struct TailscaleConfig {
    pub hostname: String,
    #[serde(skip)]
    pub auth_key: Option<Secret<String>>,
    /// Path to a file containing the auth key (alternative to TS_AUTHKEY env var)
    #[serde(default)]
    pub auth_key_file: Option<PathBuf>,
    /// Required by spec TOML schema. The sidecar approach (Option B) manages
    /// its own state via tailscaled, so this field is deserialized but not
    /// used by the Rust service directly.
    #[allow(dead_code)]
    pub state_dir: PathBuf,
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
    /// Load configuration from a TOML file, then overlay environment variables.
    ///
    /// Auth key resolution order:
    /// 1. TS_AUTHKEY env var
    /// 2. auth_key_file path from config
    pub fn load(path: &Path) -> common::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let mut config: Config = toml::from_str(&contents)?;

        // Validate upstream_url is a valid URL with http(s) scheme
        if !config.proxy.upstream_url.starts_with("http://")
            && !config.proxy.upstream_url.starts_with("https://")
        {
            return Err(common::Error::Config(format!(
                "upstream_url must start with http:// or https://, got: {}",
                config.proxy.upstream_url
            )));
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

        // Resolve auth key: env var takes precedence over file
        if let Ok(key) = std::env::var("TS_AUTHKEY") {
            config.tailscale.auth_key = Some(Secret::new(key));
        } else if let Some(ref key_file) = config.tailscale.auth_key_file {
            let key = std::fs::read_to_string(key_file).map_err(|e| {
                common::Error::Config(format!(
                    "failed to read auth_key_file {}: {e}",
                    key_file.display()
                ))
            })?;
            let key = key.trim().to_owned();
            if !key.is_empty() {
                config.tailscale.auth_key = Some(Secret::new(key));
            }
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
[tailscale]
hostname = "anthropic-oauth-proxy"
state_dir = "/var/lib/ts-state"

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

        unsafe { remove_env("TS_AUTHKEY") };

        let config = Config::load(&path).unwrap();
        assert_eq!(config.tailscale.hostname, "anthropic-oauth-proxy");
        assert_eq!(config.proxy.upstream_url, "https://api.anthropic.com");
        assert_eq!(config.proxy.timeout_secs, 60);
        assert_eq!(config.proxy.max_connections, 1000);
        assert_eq!(config.headers.len(), 1);
        assert_eq!(config.headers[0].name, "anthropic-beta");
        assert!(config.tailscale.auth_key.is_none());

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
    fn test_auth_key_from_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-env");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, valid_toml()).unwrap();

        unsafe { set_env("TS_AUTHKEY", "tskey-test-123") };
        let config = Config::load(&path).unwrap();
        assert_eq!(
            config.tailscale.auth_key.as_ref().unwrap().expose(),
            "tskey-test-123"
        );
        unsafe { remove_env("TS_AUTHKEY") };

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_auth_key_from_file() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-keyfile");
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("auth_key");
        std::fs::write(&key_path, "tskey-file-456\n").unwrap();

        let toml_content = format!(
            r#"
[tailscale]
hostname = "test"
state_dir = "/tmp"
auth_key_file = "{}"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
"#,
            key_path.display()
        );
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, &toml_content).unwrap();

        unsafe { remove_env("TS_AUTHKEY") };
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.tailscale.auth_key.as_ref().unwrap().expose(),
            "tskey-file-456"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_auth_key_env_overrides_file() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-override");
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("auth_key");
        std::fs::write(&key_path, "tskey-file-value").unwrap();

        let toml_content = format!(
            r#"
[tailscale]
hostname = "test"
state_dir = "/tmp"
auth_key_file = "{}"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
"#,
            key_path.display()
        );
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, &toml_content).unwrap();

        unsafe { set_env("TS_AUTHKEY", "tskey-env-value") };
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.tailscale.auth_key.as_ref().unwrap().expose(),
            "tskey-env-value"
        );
        unsafe { remove_env("TS_AUTHKEY") };

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
[tailscale]
hostname = "test"
state_dir = "/tmp"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
max_connections = 500
"#;
        let dir = std::env::temp_dir().join("oauth-proxy-test-maxconn");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, toml_content).unwrap();
        unsafe { remove_env("TS_AUTHKEY") };

        let config = Config::load(&path).unwrap();
        assert_eq!(config.proxy.max_connections, 500);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_auth_key_file_empty_content_yields_none() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-empty-keyfile");
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("auth_key");
        std::fs::write(&key_path, "  \n  ").unwrap(); // whitespace only

        let toml_content = format!(
            r#"
[tailscale]
hostname = "test"
state_dir = "/tmp"
auth_key_file = "{}"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
"#,
            key_path.display()
        );
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, &toml_content).unwrap();

        unsafe { remove_env("TS_AUTHKEY") };
        let config = Config::load(&config_path).unwrap();
        assert!(
            config.tailscale.auth_key.is_none(),
            "empty/whitespace-only auth_key_file should result in no auth key"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_invalid_upstream_url_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-bad-url");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[tailscale]
hostname = "test"
state_dir = "/tmp"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "api.anthropic.com"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();
        unsafe { remove_env("TS_AUTHKEY") };

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "upstream_url without scheme must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("upstream_url must start with http"),
            "error message should explain the issue, got: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_zero_timeout_rejected() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-zero-timeout");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[tailscale]
hostname = "test"
state_dir = "/tmp"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
timeout_secs = 0
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();
        unsafe { remove_env("TS_AUTHKEY") };

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
[tailscale]
hostname = "test"
state_dir = "/tmp"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
max_connections = 0
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();
        unsafe { remove_env("TS_AUTHKEY") };

        let result = Config::load(&config_path);
        assert!(result.is_err(), "max_connections = 0 must be rejected");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_auth_key_env_overrides_nonexistent_file() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-env-over-missing");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[tailscale]
hostname = "test"
state_dir = "/tmp"
auth_key_file = "/nonexistent/path/auth_key"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        unsafe { set_env("TS_AUTHKEY", "tskey-env-wins") };
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.tailscale.auth_key.as_ref().unwrap().expose(),
            "tskey-env-wins",
            "TS_AUTHKEY env var must take precedence over nonexistent auth_key_file"
        );
        unsafe { remove_env("TS_AUTHKEY") };

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_auth_key_file_nonexistent_returns_error() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let dir = std::env::temp_dir().join("oauth-proxy-test-missing-keyfile");
        std::fs::create_dir_all(&dir).unwrap();

        let toml_content = r#"
[tailscale]
hostname = "test"
state_dir = "/tmp"
auth_key_file = "/nonexistent/path/auth_key"

[proxy]
listen_addr = "127.0.0.1:8080"
upstream_url = "https://api.anthropic.com"
"#;
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, toml_content).unwrap();

        unsafe { remove_env("TS_AUTHKEY") };
        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "nonexistent auth_key_file must return an error"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
