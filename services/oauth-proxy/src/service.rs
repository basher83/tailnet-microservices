//! Service state machine
//!
//! Pure state machine: receives events, returns actions.
//! Caller handles I/O.

use std::time::Instant;

/// Service states
#[derive(Debug)]
pub enum ServiceState {
    /// Loading config, setting up resources
    Initializing,
    /// Joining the tailnet
    ConnectingTailnet { retries: u32 },
    /// Starting HTTP listener
    Starting,
    /// Accepting and proxying requests
    Running { started_at: Instant },
    /// Graceful shutdown, finishing in-flight requests
    Draining { pending_requests: u32 },
    /// Terminal state
    Stopped { exit_code: i32 },
    /// Recoverable error with retry
    Error { error: String, retries: u32 },
}

/// Events that drive state transitions
#[derive(Debug)]
pub enum ServiceEvent {
    ConfigLoaded,
    TailnetConnected,
    TailnetError(String),
    ListenerReady,
    ShutdownSignal,
    DrainComplete,
    RetryTimer,
}

/// Actions the caller should execute
#[derive(Debug)]
pub enum ServiceAction {
    LoadConfig,
    ConnectTailnet,
    StartListener,
    ScheduleRetry { delay_ms: u64 },
    Shutdown { exit_code: i32 },
    None,
}

/// Handle state transitions
pub fn handle_event(state: ServiceState, event: ServiceEvent) -> (ServiceState, ServiceAction) {
    match (state, event) {
        // Initializing
        (ServiceState::Initializing, ServiceEvent::ConfigLoaded) => (
            ServiceState::ConnectingTailnet { retries: 0 },
            ServiceAction::ConnectTailnet,
        ),

        // Connecting
        (ServiceState::ConnectingTailnet { .. }, ServiceEvent::TailnetConnected) => (
            ServiceState::Starting,
            ServiceAction::StartListener,
        ),
        (ServiceState::ConnectingTailnet { retries }, ServiceEvent::TailnetError(e)) if retries < 5 => (
            ServiceState::Error { error: e, retries },
            ServiceAction::ScheduleRetry { delay_ms: 1000 * 2u64.pow(retries) },
        ),
        (ServiceState::ConnectingTailnet { .. }, ServiceEvent::TailnetError(_)) => (
            ServiceState::Stopped { exit_code: 1 },
            ServiceAction::Shutdown { exit_code: 1 },
        ),

        // Error recovery
        (ServiceState::Error { retries, .. }, ServiceEvent::RetryTimer) => (
            ServiceState::ConnectingTailnet { retries: retries + 1 },
            ServiceAction::ConnectTailnet,
        ),

        // Starting
        (ServiceState::Starting, ServiceEvent::ListenerReady) => (
            ServiceState::Running { started_at: Instant::now() },
            ServiceAction::None,
        ),

        // Running
        (ServiceState::Running { .. }, ServiceEvent::ShutdownSignal) => (
            ServiceState::Draining { pending_requests: 0 },
            ServiceAction::None,
        ),

        // Draining
        (ServiceState::Draining { pending_requests: 0 }, ServiceEvent::DrainComplete) => (
            ServiceState::Stopped { exit_code: 0 },
            ServiceAction::Shutdown { exit_code: 0 },
        ),

        // Any state + shutdown = stop
        (_, ServiceEvent::ShutdownSignal) => (
            ServiceState::Stopped { exit_code: 0 },
            ServiceAction::Shutdown { exit_code: 0 },
        ),

        // Invalid transition - stay in current state
        (state, _) => (state, ServiceAction::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_to_connecting() {
        let (state, action) = handle_event(
            ServiceState::Initializing,
            ServiceEvent::ConfigLoaded,
        );
        assert!(matches!(state, ServiceState::ConnectingTailnet { retries: 0 }));
        assert!(matches!(action, ServiceAction::ConnectTailnet));
    }

    #[test]
    fn test_retry_backoff() {
        let (state, action) = handle_event(
            ServiceState::ConnectingTailnet { retries: 2 },
            ServiceEvent::TailnetError("timeout".into()),
        );
        assert!(matches!(state, ServiceState::Error { retries: 2, .. }));
        assert!(matches!(action, ServiceAction::ScheduleRetry { delay_ms: 4000 }));
    }

    #[test]
    fn test_max_retries_exits() {
        let (state, action) = handle_event(
            ServiceState::ConnectingTailnet { retries: 5 },
            ServiceEvent::TailnetError("timeout".into()),
        );
        assert!(matches!(state, ServiceState::Stopped { exit_code: 1 }));
        assert!(matches!(action, ServiceAction::Shutdown { exit_code: 1 }));
    }
}
