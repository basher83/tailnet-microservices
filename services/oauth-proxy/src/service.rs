//! Service state machine
//!
//! Pure state machine: receives events, returns (new_state, action).
//! Caller (main.rs) executes the I/O implied by each action.
//!
//! Spec reference: specs/oauth-proxy.md "State Machine" section.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use crate::error::Error as ServiceError;

/// Origin of an error for retry decisions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorOrigin {
    Tailnet,
}

/// Opaque handle representing an active tailnet connection.
#[derive(Debug, Clone)]
pub struct TailnetHandle {
    pub hostname: String,
    pub ip: std::net::IpAddr,
}

/// Runtime metrics tracked while the service is running
#[derive(Debug, Clone)]
pub struct ServiceMetrics {
    pub requests_total: Arc<AtomicU64>,
    pub errors_total: Arc<AtomicU64>,
    /// Number of requests currently being processed. Used for drain coordination:
    /// on shutdown, the service waits until this reaches 0 (or the drain deadline
    /// expires) before exiting.
    pub in_flight: Arc<AtomicU64>,
    pub started_at: Instant,
}

impl ServiceMetrics {
    pub fn new() -> Self {
        Self {
            requests_total: Arc::new(AtomicU64::new(0)),
            errors_total: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::new(AtomicU64::new(0)),
            started_at: Instant::now(),
        }
    }
}

/// Service states per spec.
///
/// Fields marked `dead_code` are structurally required by state transitions
/// (used in match arms for destructuring/reconstruction) but never read
/// independently. They exist because the spec defines them as state data.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ServiceState {
    /// Loading config, setting up resources
    Initializing,
    /// Joining the tailnet
    ConnectingTailnet {
        retries: u32,
        listen_addr: SocketAddr,
    },
    /// Starting HTTP listener after tailnet connected
    Starting {
        tailnet: TailnetHandle,
        listen_addr: SocketAddr,
    },
    /// Accepting and proxying requests
    Running {
        tailnet: TailnetHandle,
        listen_addr: SocketAddr,
        metrics: ServiceMetrics,
    },
    /// Graceful shutdown, finishing in-flight requests
    Draining {
        pending_requests: u32,
        deadline: Instant,
    },
    /// Terminal state
    Stopped { exit_code: i32 },
    /// Recoverable error with retry
    Error {
        error: String,
        origin: ErrorOrigin,
        retries: u32,
        listen_addr: SocketAddr,
    },
}

/// Events that drive state transitions.
///
/// Some variants are only constructed in tests (e.g. `ShutdownSignal`,
/// `DrainTimeout`, `RequestCompleted`). They exist because the spec defines
/// them and the state machine handles them; the caller (`main.rs`) delegates
/// some of these concerns to axum's built-in mechanisms instead.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ServiceEvent {
    /// Configuration parsed successfully
    ConfigLoaded { listen_addr: SocketAddr },
    /// Joined tailnet, got identity
    TailnetConnected(TailnetHandle),
    /// Failed to connect to tailnet
    TailnetError(String),
    /// HTTP listener bound and ready
    ListenerReady,
    /// Incoming HTTP request
    RequestReceived { request_id: String },
    /// Request finished (success or error)
    RequestCompleted {
        request_id: String,
        duration: Duration,
        error: Option<ServiceError>,
    },
    /// SIGTERM/SIGINT received
    ShutdownSignal,
    /// Drain deadline exceeded
    DrainTimeout,
    /// Retry backoff expired
    RetryTimer,
}

/// Actions the caller should execute after a state transition
#[derive(Debug)]
pub enum ServiceAction {
    /// Initiate tailnet connection
    ConnectTailnet,
    /// Bind HTTP listener on the given address
    StartListener { addr: SocketAddr },
    /// Set retry timer
    ScheduleRetry { delay: Duration },
    /// Exit the process
    Shutdown { exit_code: i32 },
    /// No-op
    None,
}

/// Maximum tailnet connection retries before giving up
const MAX_TAILNET_RETRIES: u32 = 5;

/// Drain timeout duration (spec: graceful shutdown <5s)
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle a state transition. Pure function: no I/O.
pub fn handle_event(state: ServiceState, event: ServiceEvent) -> (ServiceState, ServiceAction) {
    match (state, event) {
        // --- Initializing ---
        (ServiceState::Initializing, ServiceEvent::ConfigLoaded { listen_addr }) => (
            ServiceState::ConnectingTailnet {
                retries: 0,
                listen_addr,
            },
            ServiceAction::ConnectTailnet,
        ),

        // --- ConnectingTailnet ---
        (
            ServiceState::ConnectingTailnet { listen_addr, .. },
            ServiceEvent::TailnetConnected(handle),
        ) => (
            ServiceState::Starting {
                tailnet: handle,
                listen_addr,
            },
            ServiceAction::StartListener { addr: listen_addr },
        ),

        (
            ServiceState::ConnectingTailnet {
                retries,
                listen_addr,
            },
            ServiceEvent::TailnetError(e),
        ) if retries < MAX_TAILNET_RETRIES => {
            let delay = Duration::from_secs(2u64.pow(retries));
            (
                ServiceState::Error {
                    error: e,
                    origin: ErrorOrigin::Tailnet,
                    retries,
                    listen_addr,
                },
                ServiceAction::ScheduleRetry { delay },
            )
        }

        (ServiceState::ConnectingTailnet { .. }, ServiceEvent::TailnetError(_)) => (
            ServiceState::Stopped { exit_code: 1 },
            ServiceAction::Shutdown { exit_code: 1 },
        ),

        // --- Error recovery ---
        (
            ServiceState::Error {
                retries,
                origin: ErrorOrigin::Tailnet,
                listen_addr,
                ..
            },
            ServiceEvent::RetryTimer,
        ) => (
            ServiceState::ConnectingTailnet {
                retries: retries + 1,
                listen_addr,
            },
            ServiceAction::ConnectTailnet,
        ),

        // --- Starting ---
        (
            ServiceState::Starting {
                tailnet,
                listen_addr,
            },
            ServiceEvent::ListenerReady,
        ) => (
            ServiceState::Running {
                tailnet,
                listen_addr,
                metrics: ServiceMetrics::new(),
            },
            ServiceAction::None,
        ),

        // --- Running ---
        (
            ServiceState::Running { .. },
            ServiceEvent::RequestReceived { .. } | ServiceEvent::RequestCompleted { .. },
        ) => {
            // Request tracking is handled by ProxyState's atomic counters.
            // The state machine stays in Running; no action needed.
            // (We can't destructure and reconstruct Running here without moving
            // the fields, so the caller should not consume the state for these events.
            // In practice, main.rs tracks metrics directly via ProxyState.)
            //
            // This arm exists so these events don't fall through to the catch-all.
            // The caller retains ownership of the Running state.
            unreachable!(
                "RequestReceived/RequestCompleted should be handled by the caller without consuming state"
            )
        }

        (ServiceState::Running { .. }, ServiceEvent::ShutdownSignal) => {
            let deadline = Instant::now() + DRAIN_TIMEOUT;
            (
                ServiceState::Draining {
                    pending_requests: 0,
                    deadline,
                },
                ServiceAction::None,
            )
        }

        // --- Draining ---
        (
            ServiceState::Draining {
                pending_requests: 0,
                ..
            },
            ServiceEvent::RequestCompleted { .. },
        ) => (
            ServiceState::Stopped { exit_code: 0 },
            ServiceAction::Shutdown { exit_code: 0 },
        ),

        (ServiceState::Draining { .. }, ServiceEvent::DrainTimeout) => (
            ServiceState::Stopped { exit_code: 0 },
            ServiceAction::Shutdown { exit_code: 0 },
        ),

        // --- Any state + shutdown = stop ---
        (_, ServiceEvent::ShutdownSignal) => (
            ServiceState::Stopped { exit_code: 0 },
            ServiceAction::Shutdown { exit_code: 0 },
        ),

        // --- Invalid/unhandled transition: stay in current state ---
        (state, _event) => (state, ServiceAction::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn localhost_addr() -> SocketAddr {
        "127.0.0.1:8080".parse().unwrap()
    }

    fn dummy_tailnet_handle() -> TailnetHandle {
        TailnetHandle {
            hostname: "test-node".into(),
            ip: "100.64.0.1".parse().unwrap(),
        }
    }

    #[test]
    fn init_to_connecting_on_config_loaded() {
        let (state, action) = handle_event(
            ServiceState::Initializing,
            ServiceEvent::ConfigLoaded {
                listen_addr: localhost_addr(),
            },
        );
        assert!(matches!(
            state,
            ServiceState::ConnectingTailnet { retries: 0, .. }
        ));
        assert!(matches!(action, ServiceAction::ConnectTailnet));
    }

    #[test]
    fn connecting_to_starting_on_tailnet_connected() {
        let (state, action) = handle_event(
            ServiceState::ConnectingTailnet {
                retries: 0,
                listen_addr: localhost_addr(),
            },
            ServiceEvent::TailnetConnected(dummy_tailnet_handle()),
        );
        assert!(matches!(state, ServiceState::Starting { .. }));
        assert!(matches!(action, ServiceAction::StartListener { .. }));
    }

    #[test]
    fn connecting_error_triggers_retry_with_backoff() {
        let (state, action) = handle_event(
            ServiceState::ConnectingTailnet {
                retries: 2,
                listen_addr: localhost_addr(),
            },
            ServiceEvent::TailnetError("timeout".into()),
        );
        assert!(matches!(
            state,
            ServiceState::Error {
                retries: 2,
                origin: ErrorOrigin::Tailnet,
                ..
            }
        ));
        // 2^2 = 4 seconds
        assert!(
            matches!(action, ServiceAction::ScheduleRetry { delay } if delay == Duration::from_secs(4))
        );
    }

    #[test]
    fn max_retries_stops_service() {
        let (state, action) = handle_event(
            ServiceState::ConnectingTailnet {
                retries: MAX_TAILNET_RETRIES,
                listen_addr: localhost_addr(),
            },
            ServiceEvent::TailnetError("timeout".into()),
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 1 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 1 }));
    }

    #[test]
    fn error_retry_timer_returns_to_connecting() {
        let (state, action) = handle_event(
            ServiceState::Error {
                error: "timeout".into(),
                origin: ErrorOrigin::Tailnet,
                retries: 1,
                listen_addr: localhost_addr(),
            },
            ServiceEvent::RetryTimer,
        );
        assert!(matches!(
            state,
            ServiceState::ConnectingTailnet { retries: 2, .. }
        ));
        assert!(matches!(action, ServiceAction::ConnectTailnet));
    }

    #[test]
    fn starting_to_running_on_listener_ready() {
        let (state, action) = handle_event(
            ServiceState::Starting {
                tailnet: dummy_tailnet_handle(),
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ListenerReady,
        );
        assert!(matches!(state, ServiceState::Running { .. }));
        assert!(matches!(action, ServiceAction::None));
    }

    #[test]
    fn running_to_draining_on_shutdown() {
        let (state, action) = handle_event(
            ServiceState::Running {
                tailnet: dummy_tailnet_handle(),
                listen_addr: localhost_addr(),
                metrics: ServiceMetrics::new(),
            },
            ServiceEvent::ShutdownSignal,
        );
        assert!(matches!(state, ServiceState::Draining { .. }));
        assert!(matches!(action, ServiceAction::None));
    }

    #[test]
    fn draining_stops_on_drain_timeout() {
        let (state, action) = handle_event(
            ServiceState::Draining {
                pending_requests: 3,
                deadline: Instant::now(),
            },
            ServiceEvent::DrainTimeout,
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn draining_stops_when_no_pending_requests() {
        let (state, action) = handle_event(
            ServiceState::Draining {
                pending_requests: 0,
                deadline: Instant::now() + Duration::from_secs(5),
            },
            ServiceEvent::RequestCompleted {
                request_id: "req_test".into(),
                duration: Duration::from_millis(50),
                error: None,
            },
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn any_state_shutdown_signal_stops() {
        let (state, action) = handle_event(
            ServiceState::ConnectingTailnet {
                retries: 0,
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ShutdownSignal,
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn connecting_error_backoff_values_match_spec() {
        // Spec: "Exponential: 1s, 2s, 4s, 8s, 16s"
        let expected = [1, 2, 4, 8, 16];
        for (retry, &expected_secs) in expected.iter().enumerate() {
            let (_, action) = handle_event(
                ServiceState::ConnectingTailnet {
                    retries: retry as u32,
                    listen_addr: localhost_addr(),
                },
                ServiceEvent::TailnetError("test".into()),
            );
            match action {
                ServiceAction::ScheduleRetry { delay } => {
                    assert_eq!(
                        delay,
                        Duration::from_secs(expected_secs),
                        "retry {retry}: expected {expected_secs}s backoff"
                    );
                }
                ServiceAction::Shutdown { .. } => {
                    // retry 5 (index 4 is 16s, retry 5 triggers shutdown)
                    // but we only iterate 0..5, so all should be ScheduleRetry
                    panic!("unexpected shutdown at retry {retry}");
                }
                _ => panic!("unexpected action at retry {retry}: {action:?}"),
            }
        }
    }

    #[test]
    fn service_metrics_initializes_in_flight_at_zero() {
        let metrics = ServiceMetrics::new();
        assert_eq!(
            metrics.in_flight.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics
                .requests_total
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics
                .errors_total
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}
