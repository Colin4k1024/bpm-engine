/// Event payloads carried by [`EngineEvent`] variants.
pub mod payloads {
    use std::collections::HashMap;

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ProcessStarted {
        pub process_id: String,
        pub instance_id: String,
        #[serde(default)]
        pub initial_variables: Option<HashMap<String, String>>,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TokenArrived {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TokenCompleted {
        pub instance_id: String,
        pub token_id: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct UserTaskCreated {
        pub instance_id: String,
        pub node_id: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct UserTaskCompleted {
        pub task_id: String,
        pub instance_id: String,
        pub node_id: String,
        pub variables: HashMap<String, String>,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TimerFired {
        pub timer_id: String,
        pub token_id: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TimerScheduled {
        pub timer_id: String,
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
        pub fire_at: u64,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TokenFailed {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
        pub reason: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct SagaStarted {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct SagaCompleted {
        pub instance_id: String,
        pub token_id: String,
        pub node_id: String,
    }

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ProcessCompleted {
        pub instance_id: String,
    }
}

/// Immutable event driving all state transitions in the BPM engine.
///
/// Events are the sole input to [`EventHandler`](crate::EngineEvent) implementations.
/// Each event variant carries a typed payload describing what happened.
/// The engine event pump drives events through handlers until quiescence.
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

/// Reconstruct an [`EngineEvent`] from outbox columns (event_type + JSON payload).
///
/// Returns `None` if the event_type is unknown or the payload fails deserialization.
pub fn event_from_outbox(event_type: &str, payload: &str) -> Option<EngineEvent> {
    let ev = match event_type {
        "ProcessStarted" => EngineEvent::ProcessStarted(serde_json::from_str(payload).ok()?),
        "TokenArrived" => EngineEvent::TokenArrived(serde_json::from_str(payload).ok()?),
        "TokenCompleted" => EngineEvent::TokenCompleted(serde_json::from_str(payload).ok()?),
        "UserTaskCreated" => EngineEvent::UserTaskCreated(serde_json::from_str(payload).ok()?),
        "UserTaskCompleted" => EngineEvent::UserTaskCompleted(serde_json::from_str(payload).ok()?),
        "TimerFired" => EngineEvent::TimerFired(serde_json::from_str(payload).ok()?),
        "TimerScheduled" => EngineEvent::TimerScheduled(serde_json::from_str(payload).ok()?),
        "TokenFailed" => EngineEvent::TokenFailed(serde_json::from_str(payload).ok()?),
        "SagaStarted" => EngineEvent::SagaStarted(serde_json::from_str(payload).ok()?),
        "SagaCompleted" => EngineEvent::SagaCompleted(serde_json::from_str(payload).ok()?),
        "ProcessCompleted" => EngineEvent::ProcessCompleted(serde_json::from_str(payload).ok()?),
        _ => return None,
    };
    Some(ev)
}
