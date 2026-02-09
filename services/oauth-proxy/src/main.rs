//! Anthropic OAuth Proxy
//!
//! Single-binary Rust service that:
//! 1. Listens for incoming requests
//! 2. Injects required headers (anthropic-beta: oauth-2025-04-20)
//! 3. Proxies to api.anthropic.com
//!
//! Tailnet exposure is handled externally by the Tailscale Operator.

mod config;
mod metrics;
mod proxy;
mod service;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use metrics_exporter_prometheus::PrometheusHandle;
use provider::PassthroughProvider;

use crate::config::{AuthMode, Config};
use crate::proxy::ProxyState;
use crate::service::{
    DRAIN_TIMEOUT, ServiceAction, ServiceEvent, ServiceMetrics, ServiceState, handle_event,
};

/// TCP connect timeout for the upstream HTTP client (distinct from per-request timeout)
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum idle connections per host in the reqwest connection pool
const POOL_MAX_IDLE_PER_HOST: usize = 100;

/// Shared application state accessible from all handlers
#[derive(Clone)]
struct AppState {
    proxy: ProxyState,
    metrics: ServiceMetrics,
    prometheus: PrometheusHandle,
}

/// Build the axum router with all routes and shared state.
///
/// Health and metrics endpoints are outside the concurrency limit so that
/// Kubernetes probes and Prometheus scrapes are never blocked by slow proxy
/// requests occupying all `max_connections` slots.
fn build_router(state: AppState, max_connections: usize) -> Router {
    let proxy_routes = Router::new()
        .fallback(proxy_handler)
        .layer(tower::limit::ConcurrencyLimitLayer::new(max_connections));

    Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .merge(proxy_routes)
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

    let mode = config.mode();
    info!(
        listen_addr = %config.proxy.listen_addr,
        upstream_url = %config.proxy.upstream_url,
        mode = ?mode,
        headers = config.headers.len(),
        "configuration loaded"
    );

    // Transition: Initializing -> Starting
    let (new_state, action) = handle_event(
        state,
        ServiceEvent::ConfigLoaded {
            listen_addr: config.proxy.listen_addr,
        },
    );
    state = new_state;
    info!(?action, "state: Starting");

    // Execute StartListener action
    let listen_addr = match action {
        ServiceAction::StartListener { addr } => addr,
        _ => anyhow::bail!("unexpected action after ConfigLoaded: {action:?}"),
    };

    let metrics = ServiceMetrics::new();

    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .build()
        .context("failed to build HTTP client")?;

    // Construct provider based on config mode
    let provider: Arc<dyn provider::Provider> = match mode {
        AuthMode::Passthrough => {
            let headers = config
                .headers
                .iter()
                .map(|h| provider::passthrough::HeaderInjection {
                    name: h.name.clone(),
                    value: h.value.clone(),
                })
                .collect();
            Arc::new(PassthroughProvider::new(headers))
        }
        AuthMode::OAuthPool => {
            // OAuth provider will be wired in Phase 4
            anyhow::bail!("OAuth pool mode is not yet implemented");
        }
    };

    info!(provider = provider.id(), "provider initialized");

    let proxy_state = ProxyState {
        client,
        upstream_url: config.proxy.upstream_url.clone(),
        provider,
        timeout: Duration::from_secs(config.proxy.timeout_secs),
        requests_total: metrics.requests_total.clone(),
        errors_total: metrics.errors_total.clone(),
        in_flight: metrics.in_flight.clone(),
    };

    let app_state = AppState {
        proxy: proxy_state,
        metrics: metrics.clone(),
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

    info!("shutdown complete");
    Ok(())
}

/// Health endpoint: returns 200 with status, mode, uptime, requests served.
/// In OAuth mode, includes pool health details from the provider.
async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = state.metrics.started_at.elapsed().as_secs();
    let requests = state.metrics.requests_total.load(Ordering::Relaxed);
    let errors = state.metrics.errors_total.load(Ordering::Relaxed);
    let provider_health = state.proxy.provider.health().await;

    let mut body = serde_json::json!({
        "status": provider_health.status,
        "mode": state.proxy.provider.id(),
        "uptime_seconds": uptime,
        "requests_served": requests,
        "errors_total": errors,
    });

    if let Some(pool) = provider_health.pool {
        body["pool"] = pool;
    }

    (
        axum::http::StatusCode::OK,
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
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;
    use tower::ServiceExt;

    /// Global Prometheus handle shared by tests that need to verify metric output.
    /// Only one global recorder can exist per process, so tests that need to read
    /// Prometheus-rendered output after exercising `metrics::counter!()` etc. must
    /// share this handle. Tests that don't inspect rendered output use
    /// `test_prometheus_handle()` instead (isolated, non-global).
    static GLOBAL_PROMETHEUS: OnceLock<PrometheusHandle> = OnceLock::new();

    fn global_prometheus_handle() -> PrometheusHandle {
        GLOBAL_PROMETHEUS
            .get_or_init(|| crate::metrics::install_recorder())
            .clone()
    }

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
        let provider_headers: Vec<provider::passthrough::HeaderInjection> = headers
            .iter()
            .map(|h| provider::passthrough::HeaderInjection {
                name: h.name.clone(),
                value: h.value.clone(),
            })
            .collect();
        let provider: Arc<dyn provider::Provider> =
            Arc::new(provider::PassthroughProvider::new(provider_headers));

        AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: upstream_url.to_string(),
                provider,
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,
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
                    let body_bytes =
                        axum::body::to_bytes(request.into_body(), crate::proxy::MAX_BODY_SIZE)
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
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: Arc::new(AtomicU64::new(0)),
            },
            metrics,
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
        assert_eq!(json["mode"], "passthrough");
        assert_eq!(json["requests_served"], 5);
        assert_eq!(json["errors_total"], 0);
        assert!(
            json.get("tailnet").is_none(),
            "health must not include tailnet fields"
        );
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
        let errors_total = metrics.errors_total.clone();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_millis(50), // Very short timeout to trigger quickly
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,

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
                .contains("timeout"),
            "error message must mention timeout"
        );
        // Verify spec-mandated request_id field with req_ prefix
        let request_id = json["error"]["request_id"].as_str().unwrap();
        assert!(
            request_id.starts_with("req_"),
            "timeout error must include request_id with req_ prefix, got: {request_id}"
        );
        // errors_total must increment exactly once (not per retry attempt)
        assert_eq!(
            errors_total.load(Ordering::Relaxed),
            1,
            "errors_total must increment once on timeout (not per retry)"
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
        // Uses the shared global recorder so that metrics::counter!() calls
        // are captured and visible in the rendered Prometheus output.
        let handle = global_prometheus_handle();

        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: upstream_url.clone(),
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,

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

        // Trigger an upstream error to record proxy_upstream_errors_total
        let handle_err = global_prometheus_handle();
        let metrics_err = ServiceMetrics::new();
        let state_err = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: "http://127.0.0.1:1".into(),
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_secs(5),
                requests_total: metrics_err.requests_total.clone(),
                errors_total: metrics_err.errors_total.clone(),
                in_flight: metrics_err.in_flight.clone(),
            },
            metrics: metrics_err,

            prometheus: handle_err,
        };
        let app_err = build_router(state_err, 1000);
        let _ = app_err
            .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
            .await
            .unwrap();

        // Now check the /metrics endpoint output contains spec-defined metric names.
        // Rebuild the router to hit /metrics (oneshot consumes the service).
        let metrics2 = ServiceMetrics::new();
        let handle2 = global_prometheus_handle();
        let state2 = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_secs(5),
                requests_total: metrics2.requests_total.clone(),
                errors_total: metrics2.errors_total.clone(),
                in_flight: metrics2.in_flight.clone(),
            },
            metrics: metrics2,

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

        // Verify all three spec-defined metric names appear in the Prometheus output
        assert!(
            rendered.contains("proxy_requests_total"),
            "/metrics must contain proxy_requests_total.\nRendered:\n{rendered}"
        );
        assert!(
            rendered.contains("proxy_request_duration_seconds"),
            "/metrics must contain proxy_request_duration_seconds.\nRendered:\n{rendered}"
        );
        assert!(
            rendered.contains("proxy_upstream_errors_total"),
            "/metrics must contain proxy_upstream_errors_total.\nRendered:\n{rendered}"
        );

        // Verify spec-mandated label names appear alongside their metrics.
        // proxy_requests_total must have status and method labels.
        assert!(
            rendered.contains(r#"status=""#),
            "/metrics proxy_requests_total must include 'status' label.\nRendered:\n{rendered}"
        );
        assert!(
            rendered.contains(r#"method=""#),
            "/metrics proxy_requests_total must include 'method' label.\nRendered:\n{rendered}"
        );
        // proxy_upstream_errors_total must have error_type label.
        assert!(
            rendered.contains(r#"error_type=""#),
            "/metrics proxy_upstream_errors_total must include 'error_type' label.\nRendered:\n{rendered}"
        );
        // proxy_request_duration_seconds must have status label (not just globally).
        // Verify a rendered line contains both the histogram name and the status label.
        let has_duration_status = rendered.lines().any(|line| {
            line.contains("proxy_request_duration_seconds") && line.contains(r#"status=""#)
        });
        assert!(
            has_duration_status,
            "/metrics proxy_request_duration_seconds must include 'status' label on its own lines.\nRendered:\n{rendered}"
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

        // Create a body just over the limit
        let oversized = vec![b'x'; crate::proxy::MAX_BODY_SIZE + 1];
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
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_millis(50), // 50ms timeout to trigger quickly
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,

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
    async fn proxy_does_not_retry_non_timeout_errors() {
        // Per spec: only UpstreamTimeout gets retries. Connection errors (UpstreamError)
        // must NOT be retried — verify exactly 1 connection attempt for a refused connection.
        let connection_count = Arc::new(AtomicU64::new(0));
        let counter_clone = connection_count.clone();

        // Bind then immediately drop the listener to get a port that refuses connections
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        // Re-bind to count connection attempts — accept and immediately close
        drop(listener);
        let listener = TcpListener::bind(addr).await.unwrap();
        let _server = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((socket, _)) => {
                        counter_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // Close immediately — simulates connection reset, not timeout
                        drop(socket);
                    }
                    Err(_) => break,
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/no-retry-test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        // Wait briefly for connection handler to register
        tokio::time::sleep(Duration::from_millis(50)).await;

        let attempts = connection_count.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            attempts, 1,
            "non-timeout errors must NOT be retried — expected 1 attempt, got {attempts}"
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

    #[tokio::test]
    async fn health_endpoint_content_type_is_json() {
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

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            content_type, "application/json",
            "health endpoint must return application/json Content-Type"
        );
    }

    #[tokio::test]
    async fn proxy_blocks_authorization_injection_even_without_client_auth() {
        // Per spec: authorization header is protected from injection regardless
        // of whether the client sends one. If someone misconfigures an injection
        // rule for "authorization" and the client sends no auth header, the
        // injected value must NOT be applied.
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
                    // No authorization header sent by client
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

        // No authorization header should exist: the client sent none and the
        // injected one must be blocked. A strict `.is_none()` check catches any
        // regression where an authorization header leaks through from another source.
        assert!(
            json["echoed_headers"].get("authorization").is_none(),
            "authorization injection must be blocked even when client sends no auth header"
        );
        // The other injection should still work
        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "non-authorization injections must still be applied"
        );
    }

    #[tokio::test]
    async fn proxy_accepts_body_at_exact_size_limit() {
        // The proxy enforces a 10 MiB body limit. A body of exactly 10 MiB
        // must succeed, while 10 MiB + 1 byte is rejected. This boundary test
        // verifies the limit is inclusive (at-limit succeeds).
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        // Exactly at the limit should succeed
        let at_limit = vec![b'x'; crate::proxy::MAX_BODY_SIZE];
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .body(Body::from(at_limit))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a request body of exactly 10 MiB must be accepted"
        );
    }

    #[tokio::test]
    async fn proxy_preserves_multi_value_request_headers() {
        // HTTP allows multiple values for the same header name (e.g. multiple
        // Cookie or Accept-Encoding values). The proxy must preserve all values
        // using append() rather than insert() when copying headers.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        let _server = tokio::spawn(async move {
            let app =
                axum::Router::new().fallback(|request: axum::http::Request<Body>| async move {
                    // Collect all values for each header name into arrays
                    let mut headers_map: std::collections::HashMap<String, Vec<String>> =
                        std::collections::HashMap::new();
                    for (name, value) in request.headers() {
                        headers_map
                            .entry(name.to_string())
                            .or_default()
                            .push(value.to_str().unwrap_or("").to_string());
                    }
                    let body = serde_json::json!({ "headers": headers_map });
                    (StatusCode::OK, axum::Json(body))
                });
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        let app = build_router(state, 1000);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/test")
                    .header("x-multi", "value1")
                    .header("x-multi", "value2")
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

        let values = json["headers"]["x-multi"]
            .as_array()
            .expect("x-multi should be an array of values");
        assert_eq!(
            values.len(),
            2,
            "both multi-value header values must be preserved, got: {values:?}"
        );
        assert!(values.contains(&serde_json::json!("value1")));
        assert!(values.contains(&serde_json::json!("value2")));
    }

    #[tokio::test]
    async fn proxy_replaces_header_case_insensitively() {
        // HeaderMap is case-insensitive per HTTP spec. A client sending
        // "Anthropic-Beta: old" should have it replaced by the injection
        // config specifying "anthropic-beta: oauth-2025-04-20".
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
                    // Mixed-case header name — injection should still replace it
                    .header("Anthropic-Beta", "old-value")
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
            "case-insensitive header replacement must work"
        );
    }

    #[tokio::test]
    async fn proxy_streams_response_body() {
        // Verify that the proxy streams the upstream response body rather than
        // buffering it. We use a chunked upstream that sends data in parts.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        let _server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async {
                // Return a body that simulates SSE-style streamed chunks
                let body = "data: chunk1\n\ndata: chunk2\n\n";
                (
                    StatusCode::OK,
                    [("content-type", "text/event-stream")],
                    body,
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

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            content_type, "text/event-stream",
            "content-type must be preserved for SSE responses"
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body_str.contains("data: chunk1"),
            "streamed body must contain first chunk"
        );
        assert!(
            body_str.contains("data: chunk2"),
            "streamed body must contain second chunk"
        );
    }

    #[tokio::test]
    async fn metrics_error_type_label_records_connection_value() {
        // The spec requires proxy_upstream_errors_total to carry an error_type
        // label with specific values ("timeout", "connection", "invalid_request").
        // This test verifies the actual label VALUE appears in Prometheus output,
        // not just that the label name exists.
        let handle = global_prometheus_handle();

        // Trigger a connection error (502) to record error_type="connection"
        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url: "http://127.0.0.1:1".into(),
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,

            prometheus: handle.clone(),
        };
        let app = build_router(state, 1000);
        let _ = app
            .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let rendered = handle.render();
        // Verify the specific error_type label value "connection" appears
        assert!(
            rendered.contains(r#"error_type="connection""#),
            "proxy_upstream_errors_total must record error_type=\"connection\" for connection failures.\nRendered:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn metrics_records_method_label_for_post_requests() {
        // Verify that the method label in proxy_requests_total correctly reflects
        // the HTTP method used (POST in this case, not just GET).
        let handle = global_prometheus_handle();

        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,

            prometheus: handle.clone(),
        };
        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let rendered = handle.render();
        assert!(
            rendered.contains(r#"method="POST""#),
            "proxy_requests_total must record method=\"POST\" for POST requests.\nRendered:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn metrics_histogram_records_buckets() {
        // Verify that proxy_request_duration_seconds produces histogram bucket
        // lines in Prometheus output, confirming it is a real histogram (not just
        // a counter or gauge).
        let handle = global_prometheus_handle();

        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,

            prometheus: handle.clone(),
        };
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

        let rendered = handle.render();
        // With histogram buckets configured, the metric renders as a Prometheus
        // histogram with _bucket, _sum, and _count lines
        assert!(
            rendered.contains("proxy_request_duration_seconds_bucket"),
            "histogram must produce _bucket lines.\nRendered:\n{rendered}"
        );
        assert!(
            rendered.contains("proxy_request_duration_seconds_sum"),
            "histogram must produce _sum lines.\nRendered:\n{rendered}"
        );
        assert!(
            rendered.contains("proxy_request_duration_seconds_count"),
            "histogram must produce _count lines.\nRendered:\n{rendered}"
        );
        // Verify bucket lines carry the status label
        let has_bucket_with_status = rendered.lines().any(|line| {
            line.contains("proxy_request_duration_seconds_bucket{") && line.contains("status=")
        });
        assert!(
            has_bucket_with_status,
            "histogram bucket lines must include status label.\nRendered:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn proxy_protects_authorization_case_insensitively() {
        // HTTP headers are case-insensitive. The authorization protection must
        // work regardless of the casing used in the injection config. This test
        // verifies that "Authorization" (mixed case) in config is also blocked.
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
                    name: "Authorization".into(),
                    value: "Bearer INJECTED-MIXED-CASE".into(),
                },
            ],
        );

        let app = build_router(state, 1000);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messages")
                    .method("POST")
                    .header("authorization", "Bearer sk-real-token")
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
            json["echoed_headers"]["authorization"], "Bearer sk-real-token",
            "mixed-case Authorization injection must be blocked; client token must pass through"
        );
        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "non-authorization injections must still be applied"
        );
    }

    #[tokio::test]
    async fn proxy_skips_invalid_header_name_and_applies_valid_ones() {
        // If a header injection config contains an invalid header name (e.g. with
        // spaces), the proxy must skip it with a warning and still apply the
        // remaining valid injections.
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(
            &upstream_url,
            vec![
                config::HeaderInjection {
                    name: "invalid header name".into(),
                    value: "should-be-skipped".into(),
                },
                config::HeaderInjection {
                    name: "anthropic-beta".into(),
                    value: "oauth-2025-04-20".into(),
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

        // Invalid header name must be skipped, not cause a panic or error response
        assert!(
            json["echoed_headers"].get("invalid header name").is_none(),
            "invalid header name must be skipped"
        );
        // Valid injection must still be applied
        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "valid header injection must still be applied alongside invalid ones"
        );
    }

    #[tokio::test]
    async fn proxy_skips_invalid_header_value_and_applies_valid_ones() {
        // If a header injection config contains an invalid header value (e.g. with
        // control characters), the proxy must skip it and still apply the remaining
        // valid injections.
        let (upstream_url, _server) = start_echo_server().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(
            &upstream_url,
            vec![
                config::HeaderInjection {
                    name: "x-bad-value".into(),
                    value: "invalid\r\nvalue".into(),
                },
                config::HeaderInjection {
                    name: "anthropic-beta".into(),
                    value: "oauth-2025-04-20".into(),
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

        // Invalid header value must be skipped
        assert!(
            json["echoed_headers"].get("x-bad-value").is_none(),
            "header with invalid value must be skipped"
        );
        // Valid injection must still be applied
        assert_eq!(
            json["echoed_headers"]["anthropic-beta"], "oauth-2025-04-20",
            "valid header injection must still be applied alongside invalid values"
        );
    }

    #[tokio::test]
    async fn health_and_metrics_bypass_concurrency_limit() {
        // Health and metrics endpoints must respond even when the proxy's
        // concurrency limit (max_connections) is fully saturated. This ensures
        // K8s probes and Prometheus scrapes are never blocked by slow proxy traffic.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        // Slow upstream: holds connections for 2s before responding
        let _server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(|| async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                (StatusCode::OK, "slow")
            });
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let state = test_app_state(&upstream_url, vec![]);
        // max_connections=1: only one proxy request can be in-flight at a time
        let app = build_router(state, 1);

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

        // Saturate the proxy's concurrency slot with a slow request
        let proxy_req = client.get(format!("{test_url}/slow-proxy"));

        // Fire the slow proxy request (don't await yet)
        let slow_future = tokio::spawn(async move { proxy_req.send().await });
        // Give it a moment to start occupying the slot
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Health and metrics must respond immediately despite the saturated slot
        let health = client
            .get(format!("{test_url}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            health.status().as_u16(),
            200,
            "health endpoint must respond even when concurrency limit is saturated"
        );

        let metrics = client
            .get(format!("{test_url}/metrics"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            metrics.status().as_u16(),
            200,
            "metrics endpoint must respond even when concurrency limit is saturated"
        );

        // Clean up the slow request
        let _ = slow_future.await;
    }

    #[tokio::test]
    async fn proxy_resends_body_on_timeout_retry() {
        // The proxy clones body_bytes for each retry attempt. This test verifies
        // that the upstream receives the correct body on the successful retry,
        // not an empty or corrupted body.
        let received_bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bodies_clone = received_bodies.clone();
        let attempt_count = Arc::new(AtomicU64::new(0));
        let attempt_clone = attempt_count.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_url = format!("http://{addr}");

        // Upstream that times out on first attempt, succeeds on second
        let _server = tokio::spawn(async move {
            let app = axum::Router::new().fallback(move |request: axum::http::Request<Body>| {
                let bc = bodies_clone.clone();
                let ac = attempt_clone.clone();
                async move {
                    let attempt = ac.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let body_bytes =
                        axum::body::to_bytes(request.into_body(), crate::proxy::MAX_BODY_SIZE)
                            .await
                            .unwrap();
                    let body_str = String::from_utf8_lossy(&body_bytes).to_string();
                    bc.lock().unwrap().push(body_str.clone());

                    if attempt == 0 {
                        // First attempt: hang to trigger timeout
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        (StatusCode::OK, body_str).into_response()
                    } else {
                        // Subsequent attempts: respond immediately with echoed body
                        (StatusCode::OK, body_str).into_response()
                    }
                }
            });
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::new(),
                upstream_url,
                provider: Arc::new(provider::PassthroughProvider::new(vec![])),
                timeout: Duration::from_millis(50), // Short timeout to trigger retry
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,

            prometheus: test_prometheus_handle(),
        };

        let app = build_router(state, 1000);
        let request_body = r#"{"model":"claude-3","messages":[{"role":"user","content":"test"}]}"#;
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

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "retry attempt should succeed"
        );

        // Verify the response body is the echoed request body from the successful retry
        let resp_body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let resp_str = String::from_utf8(resp_body.to_vec()).unwrap();
        assert_eq!(
            resp_str, request_body,
            "successful retry must receive and echo the original request body"
        );

        // Verify the body was sent on the retry attempt (at least 2 attempts made)
        let bodies = received_bodies.lock().unwrap();
        assert!(
            bodies.len() >= 2,
            "upstream must receive body on retry attempts, got {} attempts",
            bodies.len()
        );
        // All attempts should receive the same body
        for (i, body) in bodies.iter().enumerate() {
            assert_eq!(
                body, request_body,
                "attempt {i} should receive the original request body, got: {body}"
            );
        }
    }

    /// Load test: verify the proxy sustains 100+ req/s throughput.
    ///
    /// This is a spec success criterion (specs/oauth-proxy.md "Success Criteria"):
    /// "Handles 100+ req/s sustained". The test fires 1000 requests across 50
    /// concurrent tasks through a real TCP listener to measure actual HTTP
    /// throughput including connection overhead, header injection, and response
    /// streaming. Marked `#[ignore]` because load tests are timing-sensitive and
    /// should not gate CI — run explicitly with:
    ///
    ///   cargo test -p oauth-proxy -- --ignored load_test_sustains_100_rps
    #[tokio::test]
    #[ignore]
    async fn load_test_sustains_100_rps() {
        // Start a mock upstream echo server
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_url = format!("http://{upstream_addr}");

        tokio::spawn(async move {
            let app =
                axum::Router::new().fallback(|request: axum::http::Request<Body>| async move {
                    let body_bytes =
                        axum::body::to_bytes(request.into_body(), crate::proxy::MAX_BODY_SIZE)
                            .await
                            .unwrap();
                    (StatusCode::OK, body_bytes)
                });
            axum::serve(upstream_listener, app).await.unwrap();
        });

        // Start the proxy on a real TCP listener so we measure actual HTTP throughput
        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::builder()
                    .pool_max_idle_per_host(100)
                    .build()
                    .unwrap(),
                upstream_url,
                provider: Arc::new(provider::PassthroughProvider::new(vec![
                    provider::passthrough::HeaderInjection {
                        name: "anthropic-beta".into(),
                        value: "oauth-2025-04-20".into(),
                    },
                ])),
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics: metrics.clone(),
            prometheus: test_prometheus_handle(),
        };

        let app = build_router(state, 1000);
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_url = format!("http://{proxy_addr}");

        tokio::spawn(async move {
            axum::serve(proxy_listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Fire 1000 requests across 50 concurrent tasks (20 sequential per task)
        let total_requests: u64 = 1000;
        let concurrency: u64 = 50;
        let per_task = total_requests / concurrency;
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(100)
            .build()
            .unwrap();

        let start = Instant::now();
        let mut handles = Vec::new();

        for _ in 0..concurrency {
            let client = client.clone();
            let url = format!("{proxy_url}/v1/messages");
            handles.push(tokio::spawn(async move {
                let mut ok_count = 0u64;
                for _ in 0..per_task {
                    let resp = client
                        .post(&url)
                        .header("content-type", "application/json")
                        .header("authorization", "Bearer sk-test")
                        .body(r#"{"model":"claude-3"}"#)
                        .send()
                        .await
                        .unwrap();
                    if resp.status().is_success() {
                        // Consume the body to complete the request cycle
                        let _ = resp.bytes().await;
                        ok_count += 1;
                    }
                }
                ok_count
            }));
        }

        let mut total_ok = 0u64;
        for handle in handles {
            total_ok += handle.await.unwrap();
        }
        let elapsed = start.elapsed();
        let rps = total_ok as f64 / elapsed.as_secs_f64();

        assert_eq!(
            total_ok, total_requests,
            "all {total_requests} requests must succeed, got {total_ok}"
        );
        assert!(
            rps >= 100.0,
            "spec requires 100+ req/s sustained throughput, measured {rps:.1} req/s over {:.2}s",
            elapsed.as_secs_f64()
        );

        // Also verify the proxy's internal request counter matches
        let counted = metrics
            .requests_total
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            counted, total_requests,
            "proxy request counter must match total requests sent"
        );
    }

    /// Get current process RSS (Resident Set Size) in bytes.
    ///
    /// Uses platform-specific APIs: `mach_task_basic_info` on macOS,
    /// `/proc/self/statm` on Linux. Returns None on unsupported platforms.
    fn current_rss_bytes() -> Option<usize> {
        #[cfg(target_os = "macos")]
        {
            use std::mem;
            // SAFETY: calling mach kernel API to read our own process memory stats.
            // This is a read-only query with no side effects.
            #[allow(deprecated)] // libc deprecates mach wrappers in favor of mach2 crate,
            // but mach2 v0.4 lacks the mach_task_basic_info struct definition
            unsafe {
                let task = libc::mach_task_self();
                let flavor = 5; // MACH_TASK_BASIC_INFO
                let mut info: libc::mach_task_basic_info = mem::zeroed();
                let mut count = (mem::size_of::<libc::mach_task_basic_info>()
                    / mem::size_of::<libc::natural_t>())
                    as libc::mach_msg_type_number_t;
                let kr = libc::task_info(
                    task,
                    flavor,
                    &mut info as *mut _ as libc::task_info_t,
                    &mut count,
                );
                if kr == 0 {
                    // KERN_SUCCESS
                    Some(info.resident_size as usize)
                } else {
                    None
                }
            }
        }
        #[cfg(target_os = "linux")]
        {
            // /proc/self/statm fields: size resident shared text lib data dt (in pages)
            if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
                let fields: Vec<&str> = statm.split_whitespace().collect();
                if fields.len() >= 2 {
                    if let Ok(resident_pages) = fields[1].parse::<usize>() {
                        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
                        return Some(resident_pages * page_size);
                    }
                }
            }
            None
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }

    /// Memory soak test: verify no memory leaks under sustained load.
    ///
    /// This validates the spec success criterion (specs/oauth-proxy.md "Success Criteria"):
    /// "Zero memory growth over 24h". A full 24-hour soak is impractical in CI, so this
    /// compressed version runs 20,000 requests through the proxy after a warmup phase,
    /// sampling RSS at intervals. Any per-request memory leak (retained allocations,
    /// unbounded caches, connection pool growth) would manifest as linear RSS growth
    /// across the sample windows. The test asserts that post-warmup RSS growth stays
    /// under 5 MiB — enough headroom for OS-level jitter while catching real leaks
    /// (20,000 requests with even a 256-byte-per-request leak would grow ~5 MiB).
    ///
    /// Marked `#[ignore]` because soak tests take longer than unit tests and should
    /// not gate CI. Run explicitly with:
    ///
    ///   cargo test -p oauth-proxy -- --ignored memory_soak_test_zero_growth
    #[tokio::test]
    #[ignore]
    async fn memory_soak_test_zero_growth() {
        let rss = match current_rss_bytes() {
            Some(r) => r,
            None => {
                eprintln!(
                    "skipping memory soak test: RSS measurement not supported on this platform"
                );
                return;
            }
        };
        let _ = rss; // Confirm measurement works before setup

        // Start a mock upstream echo server
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_url = format!("http://{upstream_addr}");

        tokio::spawn(async move {
            let app =
                axum::Router::new().fallback(|request: axum::http::Request<Body>| async move {
                    let body_bytes =
                        axum::body::to_bytes(request.into_body(), crate::proxy::MAX_BODY_SIZE)
                            .await
                            .unwrap();
                    (StatusCode::OK, body_bytes)
                });
            axum::serve(upstream_listener, app).await.unwrap();
        });

        // Start the proxy on a real TCP listener
        let metrics = ServiceMetrics::new();
        let state = AppState {
            proxy: ProxyState {
                client: reqwest::Client::builder()
                    .pool_max_idle_per_host(100)
                    .build()
                    .unwrap(),
                upstream_url,
                provider: Arc::new(provider::PassthroughProvider::new(vec![
                    provider::passthrough::HeaderInjection {
                        name: "anthropic-beta".into(),
                        value: "oauth-2025-04-20".into(),
                    },
                ])),
                timeout: Duration::from_secs(5),
                requests_total: metrics.requests_total.clone(),
                errors_total: metrics.errors_total.clone(),
                in_flight: metrics.in_flight.clone(),
            },
            metrics,
            prometheus: test_prometheus_handle(),
        };

        let app = build_router(state, 1000);
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_url = format!("http://{proxy_addr}");

        tokio::spawn(async move {
            axum::serve(proxy_listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(100)
            .build()
            .unwrap();

        // Warmup phase: fill connection pools, internal caches, and trigger any
        // one-time allocations so they don't skew the measurement window.
        let warmup_requests = 2000;
        for _ in 0..warmup_requests {
            let resp = client
                .post(format!("{proxy_url}/v1/messages"))
                .header("content-type", "application/json")
                .header("authorization", "Bearer sk-test")
                .body(r#"{"model":"claude-3","max_tokens":1}"#)
                .send()
                .await
                .unwrap();
            let _ = resp.bytes().await;
        }

        // Force a brief pause to let any deferred deallocation settle
        tokio::time::sleep(Duration::from_millis(100)).await;

        let rss_after_warmup = current_rss_bytes().unwrap();

        // Sustained load phase: 20,000 requests across 10 concurrent tasks
        let total_requests: u64 = 20_000;
        let concurrency: u64 = 10;
        let per_task = total_requests / concurrency;
        let mut handles = Vec::new();

        for _ in 0..concurrency {
            let client = client.clone();
            let url = format!("{proxy_url}/v1/messages");
            handles.push(tokio::spawn(async move {
                let mut ok_count = 0u64;
                for _ in 0..per_task {
                    let resp = client
                        .post(&url)
                        .header("content-type", "application/json")
                        .header("authorization", "Bearer sk-test")
                        .body(r#"{"model":"claude-3","max_tokens":1}"#)
                        .send()
                        .await
                        .unwrap();
                    let _ = resp.bytes().await;
                    ok_count += 1;
                }
                ok_count
            }));
        }

        let mut total_ok = 0u64;
        for handle in handles {
            total_ok += handle.await.unwrap();
        }

        assert_eq!(total_ok, total_requests, "all soak requests must succeed");

        // Brief pause for deferred deallocation
        tokio::time::sleep(Duration::from_millis(100)).await;

        let rss_after_soak = current_rss_bytes().unwrap();
        let growth_bytes = rss_after_soak.saturating_sub(rss_after_warmup);
        let growth_mib = growth_bytes as f64 / (1024.0 * 1024.0);

        // A 256-byte-per-request leak across 20,000 requests would grow ~5 MiB.
        // Allow 5 MiB of headroom for OS-level jitter (page reclamation timing,
        // thread stack growth, allocator fragmentation).
        let max_growth_mib = 5.0;

        eprintln!(
            "memory soak results: warmup_rss={:.1} MiB, final_rss={:.1} MiB, growth={:.2} MiB ({} requests)",
            rss_after_warmup as f64 / (1024.0 * 1024.0),
            rss_after_soak as f64 / (1024.0 * 1024.0),
            growth_mib,
            total_requests,
        );

        assert!(
            growth_mib < max_growth_mib,
            "spec requires zero memory growth under sustained load; measured {growth_mib:.2} MiB growth over {total_requests} requests (limit: {max_growth_mib} MiB). This indicates a memory leak."
        );
    }

    #[tokio::test]
    async fn listener_bind_fails_when_port_in_use() {
        // Per spec: ListenerBindError when port is already in use.
        // The bind path in main() uses TcpListener::bind with anyhow context.
        // Verify that binding to an occupied port produces an error with the address.
        let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = first.local_addr().unwrap();

        let result = TcpListener::bind(addr).await;
        assert!(result.is_err(), "binding to an occupied port must fail");
        let err = result.unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::AddrInUse,
            "error kind must be AddrInUse, got {:?}",
            err.kind()
        );
    }
}
