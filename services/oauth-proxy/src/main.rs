//! Anthropic OAuth Proxy
//!
//! Single-binary Rust service that:
//! 1. Joins tailnet with its own identity
//! 2. Listens for incoming requests
//! 3. Injects required headers (anthropic-beta: oauth-2025-04-20)
//! 4. Proxies to api.anthropic.com

mod config;
#[allow(dead_code)]
mod error;
mod proxy;
#[allow(dead_code)]
mod service;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::Config;
use crate::proxy::ProxyState;

/// Shared application state accessible from all handlers
#[derive(Clone)]
struct AppState {
    proxy: ProxyState,
    started_at: Instant,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with JSON output and LOG_LEVEL / RUST_LOG support
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_env("LOG_LEVEL")
                .or_else(|_| EnvFilter::try_from_default_env())
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    info!("starting anthropic-oauth-proxy");

    // CLI: simple --config flag parsing
    let args: Vec<String> = std::env::args().collect();
    let cli_config_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    let config_path = Config::resolve_path(cli_config_path);
    info!(path = %config_path.display(), "loading configuration");

    let config = Config::load(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    info!(
        listen_addr = %config.proxy.listen_addr,
        upstream_url = %config.proxy.upstream_url,
        hostname = %config.tailscale.hostname,
        headers = config.headers.len(),
        "configuration loaded"
    );

    let requests_total = Arc::new(AtomicU64::new(0));
    let errors_total = Arc::new(AtomicU64::new(0));

    let proxy_state = ProxyState {
        client: reqwest::Client::new(),
        upstream_url: config.proxy.upstream_url.clone(),
        headers_to_inject: config.headers.clone(),
        timeout: Duration::from_secs(config.proxy.timeout_secs),
        requests_total: requests_total.clone(),
        errors_total: errors_total.clone(),
    };

    let app_state = AppState {
        proxy: proxy_state,
        started_at: Instant::now(),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .fallback(proxy_handler)
        .with_state(app_state);

    let listener = TcpListener::bind(config.proxy.listen_addr)
        .await
        .with_context(|| format!("failed to bind to {}", config.proxy.listen_addr))?;

    info!(addr = %config.proxy.listen_addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    info!("shutdown complete");
    Ok(())
}

/// Health endpoint per spec: returns JSON with status, tailnet state, uptime, requests served.
async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = state.started_at.elapsed().as_secs();
    let requests = state.proxy.requests_total.load(Ordering::Relaxed);
    let errors = state.proxy.errors_total.load(Ordering::Relaxed);

    let body = serde_json::json!({
        "status": "healthy",
        "tailnet": "not_connected",
        "uptime_seconds": uptime,
        "requests_served": requests,
        "errors_total": errors,
    });

    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
}

/// Catch-all handler that proxies all non-health requests to upstream.
async fn proxy_handler(
    State(state): State<AppState>,
    request: axum::http::Request<axum::body::Body>,
) -> Response {
    let request_id = format!("req_{}", uuid::Uuid::new_v4().as_simple());
    proxy::proxy_request(&state.proxy, request, request_id).await
}

/// Wait for SIGTERM or SIGINT for graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}
