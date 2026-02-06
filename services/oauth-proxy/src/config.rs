//! Configuration types and loading

use common::Secret;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

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
    pub state_dir: PathBuf,
}

/// HTTP proxy settings
#[derive(Debug, Deserialize)]
pub struct ProxyConfig {
    pub listen_addr: SocketAddr,
    pub upstream_url: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

/// Header to inject
#[derive(Debug, Deserialize)]
pub struct HeaderInjection {
    pub name: String,
    pub value: String,
}

fn default_timeout() -> u64 {
    60
}

impl Config {
    /// Load configuration from file
    pub fn load(_path: &PathBuf) -> common::Result<Self> {
        // TODO: Implement config loading
        todo!("Load config from TOML file")
    }
}
