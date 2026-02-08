//! Service state machine
//!
//! Pure state machine: receives events, returns (new_state, action).
//! Caller (main.rs) executes the I/O implied by each action.
//!
//! Spec reference: specs/operator-migration.md "R6: State Machine Simplification".

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

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
/// Simplified for operator migration: no tailnet connection states.
/// The Tailscale Operator handles tailnet exposure externally.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ServiceState {
    /// Loading config, setting up resources
    Initializing,
    /// Starting HTTP listener
    Starting { listen_addr: SocketAddr },
    /// Accepting and proxying requests.
    /// Metrics are owned by `ProxyState` in main.rs, not the state machine.
    Running { listen_addr: SocketAddr },
    /// Graceful shutdown, finishing in-flight requests.
    /// Actual drain coordination is handled by axum's `with_graceful_shutdown`
    /// and the `in_flight` atomic counter in `ProxyState`. The state machine
    /// only tracks the deadline for timeout purposes.
    Draining { deadline: Instant },
    /// Terminal state
    Stopped { exit_code: i32 },
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
    /// HTTP listener bound and ready
    ListenerReady,
    /// Incoming HTTP request
    RequestReceived { request_id: String },
    /// Request finished (success or error)
    RequestCompleted {
        request_id: String,
        duration: Duration,
        error: Option<String>,
    },
    /// SIGTERM/SIGINT received
    ShutdownSignal,
    /// Drain deadline exceeded
    DrainTimeout,
}

/// Actions the caller should execute after a state transition
#[derive(Debug)]
#[allow(dead_code)]
pub enum ServiceAction {
    /// Bind HTTP listener on the given address
    StartListener { addr: SocketAddr },
    /// Exit the process
    Shutdown { exit_code: i32 },
    /// No-op
    None,
}

/// Drain timeout duration (spec: graceful shutdown <5s).
/// Used by the state machine for transition deadlines and by main.rs
/// to enforce a hard exit if in-flight requests don't complete in time.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle a state transition. Pure function: no I/O.
pub fn handle_event(state: ServiceState, event: ServiceEvent) -> (ServiceState, ServiceAction) {
    match (state, event) {
        // --- Initializing ---
        (ServiceState::Initializing, ServiceEvent::ConfigLoaded { listen_addr }) => (
            ServiceState::Starting { listen_addr },
            ServiceAction::StartListener { addr: listen_addr },
        ),

        // --- Starting ---
        (ServiceState::Starting { listen_addr }, ServiceEvent::ListenerReady) => {
            (ServiceState::Running { listen_addr }, ServiceAction::None)
        }

        // --- Running ---
        (
            ServiceState::Running { listen_addr },
            ServiceEvent::RequestReceived { .. } | ServiceEvent::RequestCompleted { .. },
        ) => {
            // Request tracking is handled by ProxyState's atomic counters,
            // not through the state machine. The caller (main.rs) should never
            // send these events. This arm returns a defensive no-op instead of
            // unreachable!() to avoid aborting the process if triggered accidentally.
            (ServiceState::Running { listen_addr }, ServiceAction::None)
        }

        (ServiceState::Running { .. }, ServiceEvent::ShutdownSignal) => {
            let deadline = Instant::now() + DRAIN_TIMEOUT;
            (ServiceState::Draining { deadline }, ServiceAction::None)
        }

        // --- Draining ---
        (ServiceState::Draining { .. }, ServiceEvent::DrainTimeout) => (
            ServiceState::Stopped { exit_code: 0 },
            ServiceAction::Shutdown { exit_code: 0 },
        ),

        // Draining + ShutdownSignal: redundant signal during drain, stop immediately
        (ServiceState::Draining { .. }, ServiceEvent::ShutdownSignal) => (
            ServiceState::Stopped { exit_code: 0 },
            ServiceAction::Shutdown { exit_code: 0 },
        ),

        // --- Stopped is terminal: all events are no-ops ---
        (ServiceState::Stopped { exit_code }, _) => {
            (ServiceState::Stopped { exit_code }, ServiceAction::None)
        }

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

    #[test]
    fn init_to_starting_on_config_loaded() {
        let (state, action) = handle_event(
            ServiceState::Initializing,
            ServiceEvent::ConfigLoaded {
                listen_addr: localhost_addr(),
            },
        );
        assert!(matches!(state, ServiceState::Starting { .. }));
        assert!(matches!(action, ServiceAction::StartListener { .. }));
    }

    #[test]
    fn non_default_listen_addr_preserved_through_transitions() {
        // Verify a non-default address flows through ConfigLoaded -> Starting -> Running
        let custom_addr: SocketAddr = "0.0.0.0:9090".parse().unwrap();
        let (state, action) = handle_event(
            ServiceState::Initializing,
            ServiceEvent::ConfigLoaded {
                listen_addr: custom_addr,
            },
        );
        assert!(matches!(
            state,
            ServiceState::Starting { listen_addr } if listen_addr == custom_addr
        ));
        assert!(matches!(
            action,
            ServiceAction::StartListener { addr } if addr == custom_addr
        ));

        let (state, _) = handle_event(state, ServiceEvent::ListenerReady);
        assert!(matches!(
            state,
            ServiceState::Running { listen_addr } if listen_addr == custom_addr
        ));
    }

    #[test]
    fn starting_to_running_on_listener_ready() {
        let (state, action) = handle_event(
            ServiceState::Starting {
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
                listen_addr: localhost_addr(),
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
                deadline: Instant::now(),
            },
            ServiceEvent::DrainTimeout,
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn any_state_shutdown_signal_stops() {
        let (state, action) = handle_event(
            ServiceState::Starting {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ShutdownSignal,
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn draining_ignores_irrelevant_events() {
        // Draining state receiving ConfigLoaded should stay in Draining.
        let (state, action) = handle_event(
            ServiceState::Draining {
                deadline: Instant::now() + Duration::from_secs(5),
            },
            ServiceEvent::ConfigLoaded {
                listen_addr: localhost_addr(),
            },
        );
        assert!(matches!(state, ServiceState::Draining { .. }));
        assert!(matches!(action, ServiceAction::None));
    }

    #[test]
    fn shutdown_signal_from_initializing_stops() {
        let (state, action) =
            handle_event(ServiceState::Initializing, ServiceEvent::ShutdownSignal);
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn shutdown_signal_from_starting_stops() {
        let (state, action) = handle_event(
            ServiceState::Starting {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ShutdownSignal,
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn shutdown_signal_from_draining_stops() {
        let (state, action) = handle_event(
            ServiceState::Draining {
                deadline: Instant::now() + Duration::from_secs(5),
            },
            ServiceEvent::ShutdownSignal,
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 0 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 0 }));
    }

    #[test]
    fn stopped_state_is_terminal() {
        // Stopped is a terminal state: all events (including ShutdownSignal) are no-ops.
        // The exit_code is preserved and no further actions are produced.
        let events = vec![
            ServiceEvent::ConfigLoaded {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ListenerReady,
            ServiceEvent::DrainTimeout,
            ServiceEvent::ShutdownSignal,
        ];
        for event in events {
            let (state, action) = handle_event(ServiceState::Stopped { exit_code: 0 }, event);
            assert!(
                matches!(state, ServiceState::Stopped { exit_code: 0 }),
                "Stopped must remain terminal"
            );
            assert!(
                matches!(action, ServiceAction::None),
                "Stopped must produce no action"
            );
        }
    }

    #[test]
    fn stopped_with_failure_exit_code_is_terminal() {
        // Stopped { exit_code: 1 } (failure) must also be terminal and preserve
        // its exit_code. This complements stopped_state_is_terminal which tests
        // exit_code: 0.
        let events = vec![
            ServiceEvent::ConfigLoaded {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ListenerReady,
            ServiceEvent::DrainTimeout,
            ServiceEvent::ShutdownSignal,
        ];
        for event in events {
            let (state, action) = handle_event(ServiceState::Stopped { exit_code: 1 }, event);
            assert!(
                matches!(state, ServiceState::Stopped { exit_code: 1 }),
                "Stopped{{exit_code: 1}} must remain terminal and preserve exit_code"
            );
            assert!(
                matches!(action, ServiceAction::None),
                "Stopped must produce no action"
            );
        }
    }

    #[test]
    fn initializing_ignores_unexpected_events() {
        // Initializing should only respond to ConfigLoaded. All other events
        // (except ShutdownSignal) should be silently ignored via the catch-all.
        let (state, action) = handle_event(ServiceState::Initializing, ServiceEvent::ListenerReady);
        assert!(
            matches!(state, ServiceState::Initializing),
            "Initializing must ignore ListenerReady"
        );
        assert!(matches!(action, ServiceAction::None));
    }

    #[test]
    fn draining_ignores_request_events() {
        // During graceful shutdown, in-flight requests completing generate
        // RequestCompleted events. The state machine must stay in Draining
        // and produce no action — drain coordination is handled by axum.
        let (state, action) = handle_event(
            ServiceState::Draining {
                deadline: Instant::now() + Duration::from_secs(5),
            },
            ServiceEvent::RequestReceived {
                request_id: "req_test".into(),
            },
        );
        assert!(matches!(state, ServiceState::Draining { .. }));
        assert!(matches!(action, ServiceAction::None));

        let (state, action) = handle_event(
            ServiceState::Draining {
                deadline: Instant::now() + Duration::from_secs(5),
            },
            ServiceEvent::RequestCompleted {
                request_id: "req_test".into(),
                duration: Duration::from_millis(100),
                error: None,
            },
        );
        assert!(matches!(state, ServiceState::Draining { .. }));
        assert!(matches!(action, ServiceAction::None));
    }

    #[test]
    fn running_handles_request_events_as_noop() {
        // Request tracking is handled by ProxyState's atomic counters, not the
        // state machine. If RequestReceived or RequestCompleted events reach the
        // state machine while Running, they must be treated as no-ops: the state
        // stays Running and no action is produced.
        let (state, action) = handle_event(
            ServiceState::Running {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::RequestReceived {
                request_id: "req_running_test".into(),
            },
        );
        assert!(
            matches!(state, ServiceState::Running { .. }),
            "Running must stay Running on RequestReceived"
        );
        assert!(matches!(action, ServiceAction::None));

        let (state, action) = handle_event(
            ServiceState::Running {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::RequestCompleted {
                request_id: "req_running_test".into(),
                duration: Duration::from_millis(50),
                error: None,
            },
        );
        assert!(
            matches!(state, ServiceState::Running { .. }),
            "Running must stay Running on RequestCompleted"
        );
        assert!(matches!(action, ServiceAction::None));
    }

    #[test]
    fn starting_ignores_unexpected_events() {
        // Starting should only respond to ListenerReady (and ShutdownSignal).
        // All other events should be silently ignored via the catch-all.
        let (state, action) = handle_event(
            ServiceState::Starting {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ConfigLoaded {
                listen_addr: localhost_addr(),
            },
        );
        assert!(
            matches!(state, ServiceState::Starting { .. }),
            "Starting must ignore ConfigLoaded"
        );
        assert!(matches!(action, ServiceAction::None));
    }

    #[test]
    fn running_ignores_lifecycle_events() {
        // Running should only respond to RequestReceived/RequestCompleted (no-op)
        // and ShutdownSignal. Events from earlier lifecycle stages (ConfigLoaded,
        // ListenerReady, DrainTimeout) must be silently ignored via the catch-all.
        let lifecycle_events = vec![
            ServiceEvent::ConfigLoaded {
                listen_addr: localhost_addr(),
            },
            ServiceEvent::ListenerReady,
            ServiceEvent::DrainTimeout,
        ];
        for event in lifecycle_events {
            let (state, action) = handle_event(
                ServiceState::Running {
                    listen_addr: localhost_addr(),
                },
                event,
            );
            assert!(
                matches!(state, ServiceState::Running { .. }),
                "Running must ignore lifecycle events from earlier states"
            );
            assert!(matches!(action, ServiceAction::None));
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
