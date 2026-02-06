//! Tailscale integration via the local API
//!
//! Connects to a running `tailscaled` daemon to obtain tailnet identity
//! (hostname, IP). This is the "Option B" sidecar approach from
//! specs/tailnet.md — the most mature path for non-Go services.
//!
//! On Linux, connects via Unix socket at `/var/run/tailscale/tailscaled.sock`.
//! On macOS, connects via TCP to the local API port.

use crate::error::{Error, Result};
use crate::service::TailnetHandle;
use std::path::Path;
use tailscale_localapi::LocalApi;
use tailscale_localapi::types::BackendState;
use tracing::{debug, info};

/// Default tailscaled socket path on Linux
const DEFAULT_SOCKET_PATH: &str = "/var/run/tailscale/tailscaled.sock";

/// Connect to the local tailscaled and obtain a `TailnetHandle`.
///
/// The `expected_hostname` is the hostname from config — we log a warning if
/// the tailnet reports a different one (misconfiguration) but use the real one.
pub async fn connect(expected_hostname: &str) -> Result<TailnetHandle> {
    let status = fetch_status().await?;

    // Verify tailscaled is in a usable state
    match &status.backend_state {
        BackendState::Running => {
            debug!("tailscaled backend state: Running");
        }
        BackendState::NeedsLogin => {
            return Err(Error::TailnetAuth);
        }
        BackendState::NeedsMachineAuth => {
            return Err(Error::TailnetMachineAuth);
        }
        BackendState::Stopped => {
            return Err(Error::TailnetNotRunning(
                "tailscaled is stopped — run `tailscale up`".into(),
            ));
        }
        BackendState::Starting => {
            return Err(Error::TailnetConnect(
                "tailscaled is still starting — will retry".into(),
            ));
        }
        BackendState::NoState => {
            return Err(Error::TailnetNotRunning(
                "tailscaled has no state — is it configured?".into(),
            ));
        }
        _ => {
            return Err(Error::TailnetConnect(format!(
                "tailscaled in unexpected state: {:?}",
                status.backend_state
            )));
        }
    }

    // Extract tailnet IP — prefer the first IPv4 address
    let ip = status
        .tailscale_ips
        .iter()
        .find(|ip| ip.is_ipv4())
        .or(status.tailscale_ips.first())
        .ok_or_else(|| Error::TailnetConnect("tailscaled reported no IP addresses".into()))?;

    let hostname = status.self_status.hostname.clone();

    if hostname != expected_hostname {
        tracing::warn!(
            expected = expected_hostname,
            actual = %hostname,
            "tailnet hostname does not match config — using actual tailnet hostname"
        );
    }

    info!(
        hostname = %hostname,
        ip = %ip,
        version = %status.version,
        "connected to tailnet"
    );

    Ok(TailnetHandle { hostname, ip: *ip })
}

/// Fetch status from the tailscaled local API, auto-detecting the transport.
async fn fetch_status() -> Result<tailscale_localapi::types::Status> {
    // On macOS, tailscaled uses a TCP port with a password file for auth.
    // On Linux, it uses a Unix domain socket.
    #[cfg(target_os = "macos")]
    {
        fetch_status_macos().await
    }

    #[cfg(not(target_os = "macos"))]
    {
        fetch_status_unix().await
    }
}

/// Connect via Unix socket (Linux and other Unix-like systems).
#[cfg(not(target_os = "macos"))]
async fn fetch_status_unix() -> Result<tailscale_localapi::types::Status> {
    let socket_path =
        std::env::var("TAILSCALE_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());

    if !Path::new(&socket_path).exists() {
        return Err(Error::TailnetNotRunning(format!(
            "tailscaled socket not found at {socket_path} — is tailscaled running?"
        )));
    }

    debug!(socket = %socket_path, "connecting to tailscaled via Unix socket");
    let client = LocalApi::new_with_socket_path(&socket_path);
    client
        .status()
        .await
        .map_err(|e| Error::TailnetConnect(format!("failed to query tailscaled local API: {e}")))
}

/// Connect via TCP on macOS. Reads the local API port and password from the
/// macOS-specific locations where Tailscale stores them.
#[cfg(target_os = "macos")]
async fn fetch_status_macos() -> Result<tailscale_localapi::types::Status> {
    // On macOS, tailscaled exposes the local API on a TCP port.
    // The port is written to a file, and a password is required.
    //
    // Standard locations:
    //   Port: ~/Library/Group Containers/io.tailscale.ipn.macos/sameuserproof-{port}
    //   Or via: /var/run/tailscale/tailscaled.sock (if using open-source CLI install)
    //
    // For the App Store / standalone macOS app, the local API is accessed
    // via the `tailscale` CLI which proxies through the system extension.
    //
    // First, try Unix socket (works with open-source CLI install on macOS too).
    let socket_from_env = std::env::var("TAILSCALE_SOCKET").ok();
    let socket_path = socket_from_env.as_deref().unwrap_or(DEFAULT_SOCKET_PATH);

    if Path::new(socket_path).exists() {
        debug!(socket = %socket_path, "connecting to tailscaled via Unix socket (macOS CLI)");
        let client = LocalApi::new_with_socket_path(socket_path);
        return client.status().await.map_err(|e| {
            Error::TailnetConnect(format!("failed to query tailscaled local API: {e}"))
        });
    }

    // Fall back to shelling out to `tailscale status --json` for the macOS app,
    // since the TCP port + password discovery is fragile across Tailscale versions.
    debug!("Unix socket not available, falling back to `tailscale status --json`");
    let output = tokio::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .await
        .map_err(|e| {
            Error::TailnetNotRunning(format!(
                "failed to run `tailscale status --json`: {e} — is tailscale CLI installed?"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::TailnetConnect(format!(
            "`tailscale status --json` failed: {stderr}"
        )));
    }

    let status: tailscale_localapi::types::Status = serde_json::from_slice(&output.stdout)
        .map_err(|e| {
            Error::TailnetConnect(format!("failed to parse tailscale status JSON: {e}"))
        })?;

    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests require a running tailscaled, so most tests here
    // validate the error paths and helper logic rather than live connections.

    #[test]
    fn default_socket_path_is_sensible() {
        assert!(DEFAULT_SOCKET_PATH.contains("tailscaled"));
    }

    #[tokio::test]
    async fn connect_fails_without_tailscaled() {
        // Unless tailscaled is actually running on the test machine,
        // connect() should return a TailnetConnect error.
        let result = connect("test-node").await;
        // We can't assert success (no tailscaled in CI), but we can assert
        // it returns an error of the right type (not a panic).
        match result {
            Ok(handle) => {
                // If tailscaled happens to be running, verify we got valid data
                assert!(!handle.hostname.is_empty());
                assert!(!handle.ip.is_unspecified());
            }
            Err(Error::TailnetConnect(_)) => { /* transient failure */ }
            Err(Error::TailnetNotRunning(_)) => { /* expected in CI — no tailscaled */ }
            Err(Error::TailnetAuth) => { /* also acceptable */ }
            Err(Error::TailnetMachineAuth) => { /* needs admin approval */ }
        }
    }
}
