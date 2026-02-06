//! Anthropic OAuth Proxy
//!
//! Single-binary Rust service that:
//! 1. Joins tailnet with its own identity
//! 2. Listens for incoming requests
//! 3. Injects required headers (anthropic-beta: oauth-2025-04-20)
//! 4. Proxies to api.anthropic.com

mod config;
mod error;
mod proxy;
mod service;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting Anthropic OAuth Proxy");

    // TODO: Load config
    // TODO: Connect to tailnet
    // TODO: Start HTTP server
    // TODO: Run service state machine

    Ok(())
}
