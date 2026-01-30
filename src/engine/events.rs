//! EngineEvent: strong-typed events and payloads (design: handler.md §4).

/// Design: handler.md §4 — strong-typed enum, not String.
/// Whitepaper §6: TimerScheduled, TokenFailed, SagaStarted, SagaCompleted.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    ProcessStarted(payloads::ProcessStarted),
    TokenArrived(payloads::TokenArrived),
    TokenCompleted(payloads::TokenCompleted),
    UserTaskCreated(payloads::UserTaskCreated),
    UserTaskCompleted(payloads::UserTaskCompleted),
    TimerFired(payloads::TimerFired),
    TimerScheduled(payloads::TimerScheduled),
    TokenFailed(payloads::TokenFailed),
    SagaStarted(payloads::SagaStarted),
    SagaCompleted(payloads::SagaCompleted),
    ProcessCompleted(payloads::ProcessCompleted),
}

/// Event payloads (design: handler.md §4.2).
pub mod payloads {
    use std::collections::HashMap;

    #[derive(Debug, Clone)]
    pub struct ProcessStarted {
        pub process_id: String,
        pub instance_id: String,
    }

    #[derive(Debug, Clone)]
    pub struct TokenArrived {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
    }

    #[derive(Debug, Clone)]
    pub struct TokenCompleted {
        pub instance_id: String,
        pub token_id: String,
    }

    #[derive(Debug, Clone)]
    pub struct UserTaskCreated {
        pub instance_id: String,
        pub node_id: String,
    }

    #[derive(Debug, Clone)]
    pub struct UserTaskCompleted {
        pub task_id: String,
        pub instance_id: String,
        pub node_id: String,
        pub variables: HashMap<String, String>,
    }

    #[derive(Debug, Clone)]
    pub struct TimerFired {
        pub timer_id: String,
        pub token_id: String,
    }

    /// Whitepaper §6: timer scheduled for fire_at.
    #[derive(Debug, Clone)]
    pub struct TimerScheduled {
        pub timer_id: String,
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
        /// Unix timestamp when timer should fire.
        pub fire_at: u64,
    }

    /// Whitepaper §6: token failed at node (reason for recovery/compensation).
    #[derive(Debug, Clone)]
    pub struct TokenFailed {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
        pub reason: String,
    }

    /// Whitepaper §6: saga (compensation flow) started.
    #[derive(Debug, Clone)]
    pub struct SagaStarted {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
    }

    /// Whitepaper §6: saga (compensation flow) completed.
    #[derive(Debug, Clone)]
    pub struct SagaCompleted {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
    }

    #[derive(Debug, Clone)]
    pub struct ProcessCompleted {
        pub instance_id: String,
    }
}
