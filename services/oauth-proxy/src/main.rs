//! Anthropic OAuth Proxy
//!
//! Single-binary Rust service that:
//! 1. Joins tailnet with its own identity
//! 2. Listens for incoming requests
//! 3. Injects required headers (anthropic-beta: oauth-2025-04-20)
//! 4. Proxies to api.anthropic.com

mod config;
mod error;
mod metrics;
mod proxy;
mod service;
mod tailnet;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use metrics_exporter_prometheus::PrometheusHandle;

use crate::config::Config;
use crate::proxy::ProxyState;
use crate::service::{
    DRAIN_TIMEOUT, ServiceAction, ServiceEvent, ServiceMetrics, ServiceState, TailnetHandle,
    handle_event,
};

/// Shared application state accessible from all handlers
#[derive(Clone)]
struct AppState {
    proxy: ProxyState,
    metrics: ServiceMetrics,
    tailnet: Option<TailnetHandle>,
    prometheus: PrometheusHandle,
}

/// Build the axum router with all routes and shared state.
///
/// Applies a concurrency limit layer based on `max_connections` to enforce
/// the spec's max concurrent request limit.
fn build_router(state: AppState, max_connections: usize) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .fallback(proxy_handler)
        .layer(tower::limit::ConcurrencyLimitLayer::new(max_connections))
        .with_state(state)
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

    // Install Prometheus metrics recorder before any metrics are emitted
    let prometheus_handle = metrics::install_recorder();

    // --- State: Initializing ---
    let mut state = ServiceState::Initializing;

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

    // Transition: Initializing -> ConnectingTailnet
    let (new_state, action) = handle_event(
        state,
        ServiceEvent::ConfigLoaded {
            listen_addr: config.proxy.listen_addr,
        },
    );
    state = new_state;
    info!(?action, "state: ConnectingTailnet");

    // Execute ConnectTailnet action with retry loop per state machine spec
    match action {
        ServiceAction::ConnectTailnet => {}
        _ => anyhow::bail!("unexpected action after ConfigLoaded: {action:?}"),
    };

    let tailnet_handle = loop {
        match tailnet::connect(&config.tailscale.hostname).await {
            Ok(handle) => break handle,
            Err(crate::error::Error::TailnetAuth) => {
                let _ = handle_event(state, ServiceEvent::TailnetError("auth failed".into()));
                anyhow::bail!("tailnet authentication failed — check TS_AUTHKEY or auth_key_file");
            }
            Err(crate::error::Error::TailnetMachineAuth) => {
                let _ = handle_event(
                    state,
                    ServiceEvent::TailnetError("machine auth needed".into()),
                );
                anyhow::bail!(
                    "tailnet needs machine authorization — approve this node in the admin console"
                );
            }
            Err(crate::error::Error::TailnetNotRunning(msg)) => {
                // tailscaled not running/installed is not retryable — bail immediately
                let _ = handle_event(state, ServiceEvent::TailnetError(msg.clone()));
                anyhow::bail!("tailscaled not running: {msg}");
            }
            Err(crate::error::Error::TailnetConnect(msg)) => {
                let (new_state, action) =
                    handle_event(state, ServiceEvent::TailnetError(msg.clone()));
                state = new_state;

                match action {
                    ServiceAction::ScheduleRetry { delay } => {
                        warn!(
                            error = %msg,
                            retry_in_secs = delay.as_secs(),
                            "tailnet connection failed, retrying"
                        );
                        tokio::time::sleep(delay).await;

                        // RetryTimer transitions Error -> ConnectingTailnet
                        let (new_state, _) = handle_event(state, ServiceEvent::RetryTimer);
                        state = new_state;
                    }
                    ServiceAction::Shutdown { exit_code } => {
                        error!(error = %msg, "tailnet connection failed after max retries");
                        std::process::exit(exit_code);
                    }
                    _ => anyhow::bail!("tailnet connection failed: {msg}"),
                }
            }
        }
    };

    // Record tailnet connection in Prometheus
    metrics::set_tailnet_connected(true);

    // Transition: ConnectingTailnet -> Starting
    let (new_state, action) = handle_event(
        state,
        ServiceEvent::TailnetConnected(tailnet_handle.clone()),
    );
    state = new_state;
    info!(?action, "state: Starting");

    // Execute StartListener action
    let listen_addr = match action {
        ServiceAction::StartListener { addr } => addr,
        _ => anyhow::bail!("unexpected action after TailnetConnected: {action:?}"),
    };

    let metrics = ServiceMetrics::new();

    let proxy_state = ProxyState {
        client: reqwest::Client::new(),
        upstream_url: config.proxy.upstream_url.clone(),
        headers_to_inject: config.headers.clone(),
        timeout: Duration::from_secs(config.proxy.timeout_secs),
        requests_total: metrics.requests_total.clone(),
        errors_total: metrics.errors_total.clone(),
        in_flight: metrics.in_flight.clone(),
    };

    let app_state = AppState {
        proxy: proxy_state,
        metrics: metrics.clone(),
        tailnet: Some(tailnet_handle),
        prometheus: prometheus_handle,
    };

    let app = build_router(app_state, config.proxy.max_connections);

    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind to {listen_addr}"))?;

    // Transition: Starting -> Running
    let (_state, _action) = handle_event(state, ServiceEvent::ListenerReady);
    info!(addr = %listen_addr, "state: Running — accepting requests");

    // Clone in_flight counter for drain observability after shutdown
    let in_flight = metrics.in_flight.clone();

    // Graceful shutdown with drain timeout enforcement per spec:
    // 1. shutdown_signal() fires on SIGTERM/SIGINT
    // 2. axum stops accepting new connections and drains in-flight requests
    // 3. We enforce DRAIN_TIMEOUT so a slow client cannot block process exit
    //
    // The drain timeout starts when the shutdown signal fires, not when the
    // server starts. We achieve this by notifying the server to drain, then
    // racing the drain against the timeout.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });

    // Wait for the OS signal
    shutdown_signal().await;

    // Signal the server to begin draining
    let _ = shutdown_tx.send(());

    // Now enforce the drain timeout — this timer starts at signal receipt
    match tokio::time::timeout(DRAIN_TIMEOUT, server_handle).await {
        Ok(Ok(Ok(()))) => {
            info!("all in-flight requests drained");
        }
        Ok(Ok(Err(e))) => {
            error!(error = %e, "server error during shutdown");
        }
        Ok(Err(e)) => {
            error!(error = %e, "server task panicked");
        }
        Err(_) => {
            let remaining = in_flight.load(Ordering::Relaxed);
            warn!(
                remaining,
                drain_timeout_secs = DRAIN_TIMEOUT.as_secs(),
                "drain timeout exceeded, forcing shutdown"
            );
        }
    }

    // Mark tailnet as disconnected in Prometheus before shutting down
    metrics::set_tailnet_connected(false);

    info!("shutdown complete");
    Ok(())
}

/// Health endpoint per spec: returns JSON with status, tailnet state, uptime, requests served.
/// Returns 200 when tailnet is connected, 503 when degraded (no tailnet).
async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = state.metrics.started_at.elapsed().as_secs();
    let requests = state.metrics.requests_total.load(Ordering::Relaxed);
    let errors = state.metrics.errors_total.load(Ordering::Relaxed);

    let (status_code, body) = match &state.tailnet {
        Some(handle) => (
            axum::http::StatusCode::OK,
            serde_json::json!({
                "status": "healthy",
                "tailnet": "connected",
                "tailnet_hostname": handle.hostname,
                "tailnet_ip": handle.ip.to_string(),
                "uptime_seconds": uptime,
                "requests_served": requests,
                "errors_total": errors,
            }),
        ),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({
                "status": "degraded",
                "tailnet": "not_connected",
                "uptime_seconds": uptime,
                "requests_served": requests,
                "errors_total": errors,
            }),
        ),
    };

    (
        status_code,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
}

/// Prometheus metrics endpoint — returns metrics in text exposition format.
async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.prometheus.render(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tower::ServiceExt;

    /// Create a PrometheusHandle for tests without installing a global recorder.
    /// Using build_recorder() avoids the "recorder already installed" panic when
    /// multiple tests run in the same process.
    fn test_prometheus_handle() -> PrometheusHandle {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        recorder.handle()
    }

    /// Build test app state pointing at the given upstream URL with specified headers.
    fn test_app_state(upstream_url: &str, headers: Vec<config::HeaderInjection>) -> AppState {
        let metrics = ServiceMetrics::new();

        AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: upstream_url.to_string(),
                headers_to_inject: headers,
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,
            tailnet: Some(TailnetHandle {
                hostname: "test-proxy".into(),
                ip: "100.64.0.1".parse().unwrap(),
            }),
            prometheus: test_prometheus_handle(),
        }
    }

    /// Start a mock upstream server that echoes back request headers and body as JSON.
    async fn start_echo_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");

        let handle = tokio::spawn(async move {
            let app =
                axum::Router::new().fallback(|request: axum::http::Request<Body>| async move {
                    let mut headers_map = serde_json::Map::new();
                    for (name, value) in request.headers() {
                        headers_map.insert(
                            name.to_string(),
                            serde_json::Value::String(value.to_str().unwrap_or("").to_string()),
                        );
                    }
                    let method = request.method().to_string();
                    let path = request.uri().path().to_string();
                    let query = request.uri().query().unwrap_or("").to_string();
                    let body_bytes = axum::body::to_bytes(request.into_body(), 10 * 1024 * 1024)
                        .await
                        .unwrap();
                    let body_str = String::from_utf8_lossy(&body_bytes).to_string();
                    let body = serde_json::json!({
                        "echoed_headers": headers_map,
                        "method": method,
                        "path": path,
                        "query": query,
                        "body": body_str,
                    });
                    (
                        StatusCode::OK,
                        [("x-upstream-echo", "true")],
                        axum::Json(body),
                    )
                });
            axum::serve(listener, app).await.unwrap();
        });

        (url, handle)
    }

    #[tokio::test]
    async fn health_endpoint_returns_json() {
        let metrics = ServiceMetrics::new();
        // Increment requests counter to verify it appears in health response
        metrics
            .requests_total
            .fetch_add(5, std::sync::atomic::Ordering::Relaxed);

        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: "http://unused".into(),
                headers_to_inject: vec![],
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: Arc::new(AtomicU64::new(0)),
            },
            metrics,
            tailnet: Some(TailnetHandle {
                hostname: "test-node".into(),
                ip: "100.64.0.1".parse().unwrap(),
            }),
            prometheus: test_prometheus_handle(),
        };

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "healthy");
        assert_eq!(json["tailnet"], "connected");
        assert_eq!(json["tailnet_hostname"], "test-node");
        assert_eq!(json["requests_served"], 5);
    }

    #[tokio::test]
    async fn proxy_injects_headers_and_forwards() {
        let (upstream_url, _server) = start_echo_server().await;
        // Give the echo server a moment to bind
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(
            &upstream_url,
            vec![config::HeaderInjection {
                name: "anthropic-beta".into(),
                value: "oauth-2025-04-20".into(),
            }],
        );

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer sk-test")
                    .body(Body::from(r#"{"model":"claude-3"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Verify the injected header reached upstream
        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "anthropic-beta header should be injected"
        );
        // Verify authorization header passes through unchanged
        assert_eq!(
            json["echoed_headers"]["authorization"], "Bearer sk-test",
            "authorization header should pass through"
        );
        // Verify path is forwarded
        assert_eq!(json["path"], "/v1/messages");
        assert_eq!(json["method"], "POST");
    }

    #[tokio::test]
    async fn proxy_strips_hop_by_hop_headers() {
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("connection", "keep-alive")
                    .header("x-custom", "preserved")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // connection is hop-by-hop and should be stripped
        assert!(
            json["echoed_headers"].get("connection").is_none(),
            "hop-by-hop 'connection' header should be stripped"
        );
        // custom header should pass through
        assert_eq!(json["echoed_headers"]["x-custom"], "preserved");
    }

    #[tokio::test]
    async fn proxy_returns_502_for_dead_upstream() {
        // Point at an unreachable upstream to trigger a connection error
        let state = test_app_state("http://127.0.0.1:1", vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
            .await
            .unwrap();

        // Connection refused → 502 Bad Gateway
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["type"], "proxy_error");
    }

    #[tokio::test]
    async fn health_endpoint_without_tailnet_returns_not_connected() {
        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: "http://unused".into(),
                headers_to_inject: vec![],
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: Arc::new(AtomicU64::new(0)),
            },
            metrics,
            tailnet: None,
            prometheus: test_prometheus_handle(),
        };

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "health endpoint must return 503 when tailnet is not connected"
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "degraded");
        assert_eq!(json["tailnet"], "not_connected");
        assert!(json.get("tailnet_hostname").is_none());
        assert!(json.get("tailnet_ip").is_none());
    }

    #[tokio::test]
    async fn health_endpoint_includes_uptime_seconds() {
        let state = test_app_state("http://unused", vec![]);
        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(
            json.get("uptime_seconds").is_some(),
            "health response must include uptime_seconds"
        );
        assert!(json["uptime_seconds"].is_u64());
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_prometheus_format() {
        let state = test_app_state("http://unused", vec![]);
        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/plain"),
            "metrics endpoint must return text/plain Prometheus format"
        );
    }

    #[tokio::test]
    async fn proxy_error_response_includes_all_spec_fields() {
        let state = test_app_state("http://127.0.0.1:1", vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let error = &json["error"];
        assert_eq!(error["type"], "proxy_error");
        assert!(
            error.get("message").is_some(),
            "error response must include message"
        );
        assert!(
            error["message"].is_string(),
            "error message must be a string"
        );
        assert!(
            error.get("request_id").is_some(),
            "error response must include request_id"
        );
        let request_id = error["request_id"].as_str().unwrap();
        assert!(
            request_id.starts_with("req_"),
            "request_id must start with 'req_' prefix, got: {request_id}"
        );
    }

    #[tokio::test]
    async fn proxy_replaces_existing_header_with_injected_value() {
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(
            &upstream_url,
            vec![config::HeaderInjection {
                name: "anthropic-beta".into(),
                value: "oauth-2025-04-20".into(),
            }],
        );

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    // Send a request that already has the header with a different value
                    .header("anthropic-beta", "old-value-should-be-replaced")
                    .header("authorization", "Bearer sk-test")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // The injected value must replace the original, not append
        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "injected header must replace existing value"
        );
    }

    #[tokio::test]
    async fn proxy_passes_through_upstream_non_2xx_responses() {
        // Start a mock upstream that returns 429 Too Many Requests
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        let _server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    r#"{"error":{"type":"rate_limit_error","message":"rate limited"}}"#,
                )
            });
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // The proxy must pass through the upstream's 429 status, not wrap it
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "non-2xx upstream status must pass through unchanged"
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // The upstream's error body must pass through verbatim
        assert_eq!(json["error"]["type"], "rate_limit_error");
    }

    #[tokio::test]
    async fn proxy_timeout_returns_504_gateway_timeout() {
        // Start a server that never responds (hangs for 10s)
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        let _server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    // Accept connection but never respond — simulates timeout
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(socket);
                });
            }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                headers_to_inject: vec![],
                timeout: Duration::from_millis(50), // Very short timeout to trigger quickly
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,
            tailnet: None,
            prometheus: test_prometheus_handle(),
        };

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/timeout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Timeout after retries → 504 Gateway Timeout
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["type"], "proxy_error");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("timeout")
        );
    }

    #[tokio::test]
    async fn proxy_forwards_request_body_to_upstream() {
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let request_body = r#"{"model":"claude-3","messages":[{"role":"user","content":"hello"}]}"#;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Verify the request body was forwarded verbatim to the upstream
        assert_eq!(
            json["body"], request_body,
            "request body must be forwarded to upstream unchanged"
        );
    }

    #[tokio::test]
    async fn proxy_query_string_forwarded_to_upstream() {
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages?beta=true&version=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["path"], "/v1/messages");
        assert_eq!(
            json["query"], "beta=true&version=2",
            "query string must be forwarded to upstream"
        );
    }

    #[tokio::test]
    async fn proxy_increments_metrics_counters() {
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let requests_total = state.proxy.requests_total.clone();
        let in_flight = state.proxy.in_flight.clone();
        let app = build_router(state, 1000);

        // Before any request, counters should be zero
        assert_eq!(requests_total.load(Ordering::Relaxed), 0);
        assert_eq!(in_flight.load(Ordering::Relaxed), 0);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        // After request completes, requests_total should be incremented
        assert_eq!(
            requests_total.load(Ordering::Relaxed),
            1,
            "requests_total should be incremented after a request"
        );
        // in_flight should be back to 0 after the request completes (RAII guard)
        assert_eq!(
            in_flight.load(Ordering::Relaxed),
            0,
            "in_flight should return to 0 after request completes"
        );
    }

    #[tokio::test]
    async fn proxy_increments_errors_total_on_upstream_failure() {
        let state = test_app_state("http://127.0.0.1:1", vec![]);
        let errors_total = state.proxy.errors_total.clone();
        let app = build_router(state, 1000);

        assert_eq!(errors_total.load(Ordering::Relaxed), 0);

        let response = app
            .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            errors_total.load(Ordering::Relaxed),
            1,
            "errors_total should be incremented on upstream failure"
        );
    }

    #[tokio::test]
    async fn proxy_injects_multiple_headers() {
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(
            &upstream_url,
            vec![
                config::HeaderInjection {
                    name: "anthropic-beta".into(),
                    value: "oauth-2025-04-20".into(),
                },
                config::HeaderInjection {
                    name: "x-custom-tracking".into(),
                    value: "proxy-injected".into(),
                },
            ],
        );

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "first injected header must reach upstream"
        );
        assert_eq!(
            json["echoed_headers"]["x-custom-tracking"], "proxy-injected",
            "second injected header must reach upstream"
        );
    }

    #[tokio::test]
    async fn proxy_forwards_upstream_response_headers() {
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        // The echo server sets x-upstream-echo: true — verify it passes through
        assert_eq!(
            response.headers().get("x-upstream-echo").unwrap(),
            "true",
            "upstream response headers must be forwarded to client"
        );
    }

    #[tokio::test]
    async fn proxy_strips_hop_by_hop_from_response() {
        // Start a mock upstream that returns a hop-by-hop header in the response.
        // We use "proxy-authenticate" (a hop-by-hop header per RFC 2616) rather
        // than "transfer-encoding" which is handled at the HTTP transport layer
        // and causes connection errors when set manually.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        let _server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async {
                (
                    StatusCode::OK,
                    [
                        ("x-legit-header", "keep-me"),
                        ("proxy-authenticate", "Basic realm=test"),
                    ],
                    "ok",
                )
            });
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-legit-header").unwrap(),
            "keep-me",
            "non-hop-by-hop response headers must pass through"
        );
        assert!(
            response.headers().get("proxy-authenticate").is_none(),
            "hop-by-hop 'proxy-authenticate' header must be stripped from response"
        );
    }

    #[tokio::test]
    async fn metrics_endpoint_contains_spec_metric_names_after_request() {
        // The Prometheus recorder must be installed globally for metrics::counter!()
        // calls to actually record. Use a Once guard since only one global recorder
        // can exist per process. Other tests that don't need the global recorder
        // use test_prometheus_handle() which creates an isolated (non-global) instance.
        use std::sync::OnceLock;
        static GLOBAL_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

        let handle = GLOBAL_HANDLE
            .get_or_init(|| {
                metrics_exporter_prometheus::PrometheusBuilder::new()
                    .install_recorder()
                    .expect("failed to install test Prometheus recorder")
            })
            .clone();

        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: upstream_url.clone(),
                headers_to_inject: vec![],
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,
            tailnet: None,
            prometheus: handle,
        };

        let app = build_router(state, 1000);

        // Make a proxied request to trigger metric recording
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Now check the /metrics endpoint output contains spec-defined metric names.
        // Rebuild the router to hit /metrics (oneshot consumes the service).
        let metrics2 = ServiceMetrics::new();
        let handle2 = GLOBAL_HANDLE.get().unwrap().clone();
        let state2 = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                headers_to_inject: vec![],
                timeout: Duration::from_secs(5),
                requests_total: metrics2.requests_total.clone(),
                errors_total: metrics2.errors_total.clone(),
                in_flight: metrics2.in_flight.clone(),
            },
            metrics: metrics2,
            tailnet: None,
            prometheus: handle2,
        };
        let app2 = build_router(state2, 1000);
        let metrics_response = app2
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(metrics_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let rendered = String::from_utf8(body.to_vec()).unwrap();

        // Verify all four spec-defined metric names appear in the Prometheus output
        assert!(
            rendered.contains("proxy_requests_total"),
            "/metrics must contain proxy_requests_total after a proxied request.\nRendered:\n{rendered}"
        );
        assert!(
            rendered.contains("proxy_request_duration_seconds"),
            "/metrics must contain proxy_request_duration_seconds after a proxied request.\nRendered:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn proxy_rejects_oversized_request_body() {
        // The proxy enforces a 10MB body limit. Sending >10MB should return 400.
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let errors_total = state.proxy.errors_total.clone();
        let app = build_router(state, 1000);

        // Create a body just over 10MB
        let oversized = vec![b'x'; 10 * 1024 * 1024 + 1];
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .body(Body::from(oversized))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "requests exceeding 10MB body limit must be rejected with 400"
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["type"], "proxy_error");
        let request_id = json["error"]["request_id"].as_str().unwrap();
        assert!(
            request_id.starts_with("req_"),
            "oversized body error must include request_id with req_ prefix, got: {request_id}"
        );
        assert_eq!(
            errors_total.load(Ordering::Relaxed),
            1,
            "errors_total must increment on oversized request"
        );
    }

    #[tokio::test]
    async fn concurrency_limit_queues_excess_requests() {
        // Tower's ConcurrencyLimitLayer queues (not rejects) excess requests.
        // With max_connections=1, a second concurrent request is queued until
        // the first completes — both eventually succeed.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        // Slow upstream: holds the connection for 500ms before responding
        let _server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                (StatusCode::OK, "slow")
            });
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);

        // max_connections=1: only one request at a time
        let app = build_router(state, 1);

        // We need to bind to a real TCP port because tower::ServiceExt::oneshot
        // consumes the service. Instead, use into_make_service and send real HTTP.
        let test_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let test_addr = test_listener.local_addr().unwrap();
        let test_url = format!("http://{test_addr}");

        tokio::spawn(async move {
            axum::serve(test_listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        // Fire two requests concurrently: the first should succeed (after 500ms),
        // the second should be rejected (503) or queued. Tower's ConcurrencyLimitLayer
        // queues excess requests by default but may shed load.
        let req1 = client.get(format!("{test_url}/slow1")).send();
        let req2 = client.get(format!("{test_url}/slow2")).send();

        let (r1, r2) = tokio::join!(req1, req2);
        let s1 = r1.unwrap().status();
        let s2 = r2.unwrap().status();

        // Both should succeed because Tower's ConcurrencyLimit queues (not rejects).
        // The important thing is that the limit layer is applied and both complete.
        assert!(
            s1.is_success() && s2.is_success(),
            "both requests should eventually complete (queued, not rejected). s1={s1}, s2={s2}"
        );
    }

    #[tokio::test]
    async fn proxy_retries_timeout_exactly_three_attempts() {
        // Verify the proxy makes exactly 3 connection attempts (1 initial + 2 retries)
        // when the upstream times out. We track this via an atomic counter on the server.
        let connection_count = Arc::new(AtomicU64::new(0));
        let counter_clone = connection_count.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        // Slow upstream that never responds (triggers timeout) but counts connections
        let _server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let cc = counter_clone.clone();
                tokio::spawn(async move {
                    cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // Hold connection open but never respond
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(socket);
                });
            }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                headers_to_inject: vec![],
                timeout: Duration::from_millis(50), // 50ms timeout to trigger quickly
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,
            tailnet: None,
            prometheus: test_prometheus_handle(),
        };

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/timeout-retry-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);

        // Wait briefly for all connection handlers to register
        tokio::time::sleep(Duration::from_millis(50)).await;

        let attempts = connection_count.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            attempts, 3,
            "proxy must make exactly 3 attempts (1 initial + 2 retries) on timeout, got {attempts}"
        );
    }

    #[tokio::test]
    async fn proxy_strips_host_header_before_forwarding() {
        // The client's Host header carries the proxy's hostname, but the upstream
        // expects its own hostname. Reqwest sets the correct Host from the URL,
        // so we must strip the client's Host before forwarding.
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .header("host", "anthropic-oauth-proxy.tailnet:443")
                    .header("authorization", "Bearer sk-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // The host header reaching the upstream must be the upstream's own address,
        // not the proxy's tailnet hostname that the client sent
        let echoed_host = json["echoed_headers"]["host"].as_str().unwrap();
        assert!(
            !echoed_host.contains("anthropic-oauth-proxy"),
            "proxy must strip client's host header so upstream receives its own host, got: {echoed_host}"
        );
    }

    #[tokio::test]
    async fn proxy_protects_authorization_from_injection() {
        // Per spec: authorization header must pass through unchanged regardless
        // of injection configuration. Even if someone misconfigures an injection
        // rule for "authorization", the client's Bearer token must not be overwritten.
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(
            &upstream_url,
            vec![
                config::HeaderInjection {
                    name: "anthropic-beta".into(),
                    value: "oauth-2025-04-20".into(),
                },
                config::HeaderInjection {
                    name: "authorization".into(),
                    value: "Bearer INJECTED-SHOULD-NOT-APPEAR".into(),
                },
            ],
        );

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .header("authorization", "Bearer sk-real-user-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // The client's authorization must pass through, not be replaced by injection
        assert_eq!(
            json["echoed_headers"]["authorization"], "Bearer sk-real-user-token",
            "authorization header must pass through unchanged per spec, injection must not overwrite it"
        );
        // The other injection should still work
        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "non-authorization injections must still be applied"
        );
    }
}
