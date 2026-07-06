/// Event payloads carried by [`EngineEvent`] variants.
pub mod payloads {
    use std::collections::HashMap;

    /// Payload for [`super::EngineEvent::ProcessStarted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ProcessStarted {
        /// The process definition ID that was started.
        pub process_id: String,
        /// The newly created process instance ID.
        pub instance_id: String,
        /// Optional initial variables injected at process start.
        #[serde(default)]
        pub initial_variables: Option<HashMap<String, String>>,
    }

    /// Payload for [`super::EngineEvent::TokenArrived`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TokenArrived {
        /// The process instance containing the token.
        pub instance_id: String,
        /// The token that arrived at the node.
        pub token_id: String,
        /// The node the token arrived at.
        pub node_id: String,
    }

    /// Payload for [`super::EngineEvent::TokenCompleted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TokenCompleted {
        /// The process instance containing the token.
        pub instance_id: String,
        /// The token that completed execution.
        pub token_id: String,
    }

    /// Payload for [`super::EngineEvent::UserTaskCreated`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct UserTaskCreated {
        /// The process instance containing the user task.
        pub instance_id: String,
        /// The user task node ID.
        pub node_id: String,
    }

    /// Payload for [`super::EngineEvent::UserTaskCompleted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct UserTaskCompleted {
        /// The user task identifier.
        pub task_id: String,
        /// The process instance containing the user task.
        pub instance_id: String,
        /// The user task node ID.
        pub node_id: String,
        /// Variables submitted by the user upon completion.
        pub variables: HashMap<String, String>,
    }

    /// Payload for [`super::EngineEvent::TimerFired`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TimerFired {
        /// The timer record ID that fired.
        pub timer_id: String,
        /// The token waiting on this timer.
        pub token_id: String,
        /// The node ID the timer was scheduled for (needed for boundary events).
        #[serde(default)]
        pub node_id: String,
    }

    /// Payload for [`super::EngineEvent::TimerScheduled`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TimerScheduled {
        /// The timer record ID.
        pub timer_id: String,
        /// The process instance owning the timer.
        pub instance_id: String,
        /// The token waiting on this timer.
        pub token_id: String,
        /// The node the timer is attached to.
        pub node_id: String,
        /// Unix timestamp (seconds) when the timer should fire.
        pub fire_at: u64,
    }

    /// Payload for [`super::EngineEvent::TokenFailed`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct TokenFailed {
        /// The process instance containing the failed token.
        pub instance_id: String,
        /// The token that failed.
        pub token_id: String,
        /// The node where the failure occurred.
        pub node_id: String,
        /// Human-readable failure reason.
        pub reason: String,
    }

    /// Payload for [`super::EngineEvent::SagaStarted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct SagaStarted {
        /// The process instance entering saga mode.
        pub instance_id: String,
        /// The token triggering the saga.
        pub token_id: String,
        /// The saga boundary node ID.
        pub node_id: String,
    }

    /// Payload for [`super::EngineEvent::SagaCompleted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct SagaCompleted {
        /// The process instance whose saga completed.
        pub instance_id: String,
        /// The token that triggered the saga.
        pub token_id: String,
        /// The saga boundary node ID.
        pub node_id: String,
    }

    /// Payload for [`super::EngineEvent::ProcessCompleted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ProcessCompleted {
        /// The process instance that completed.
        pub instance_id: String,
    }

    /// Payload for [`super::EngineEvent::CallActivityStarted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct CallActivityStarted {
        /// The parent process instance ID.
        pub parent_instance_id: String,
        /// The parent token to resume when the child completes.
        pub parent_token_id: String,
        /// The newly created child process instance ID.
        pub child_instance_id: String,
        /// The process definition key of the child process.
        pub child_process_key: String,
    }

    /// Payload for [`super::EngineEvent::CallActivityCompleted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct CallActivityCompleted {
        /// The parent process instance ID.
        pub parent_instance_id: String,
        /// The parent token to resume.
        pub parent_token_id: String,
        /// The child process instance that completed.
        pub child_instance_id: String,
    }

    /// Payload for [`super::EngineEvent::MessageSent`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct MessageSent {
        /// The process instance sending the message.
        pub instance_id: String,
        /// The message name.
        pub message_name: String,
    }

    /// Payload for [`super::EngineEvent::SignalSent`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct SignalSent {
        /// The process instance sending the signal.
        pub instance_id: String,
        /// The signal name (global broadcast).
        pub signal_name: String,
    }

    /// Payload for [`super::EngineEvent::ProcessTerminated`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ProcessTerminated {
        /// The process instance that was terminated.
        pub instance_id: String,
    }

    /// Payload for [`super::EngineEvent::ExternalTaskCompleted`].
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ExternalTaskCompleted {
        /// The process instance containing the external task.
        pub instance_id: String,
        /// The token associated with the external task.
        pub token_id: String,
        /// The node where the external task was defined.
        pub node_id: String,
        /// Variables returned by the external worker upon completion.
        #[serde(default)]
        pub variables: HashMap<String, String>,
    }
}

/// Immutable event driving all state transitions in the BPM engine.
///
/// Events are the sole input to [`EventHandler`](crate::EngineEvent) implementations.
/// Each event variant carries a typed payload describing what happened.
/// The engine event pump drives events through handlers until quiescence.
///
/// # Example
///
/// ```
/// use bpm_engine_core::EngineEvent;
///
/// let event = EngineEvent::ProcessStarted(
///     bpm_engine_core::event::payloads::ProcessStarted {
///         process_id: "order-flow".into(),
///         instance_id: "inst-1".into(),
///         initial_variables: None,
///     }
/// );
///
/// // Events are cloneable and debuggable
/// let cloned = event.clone();
/// assert!(format!("{:?}", cloned).contains("ProcessStarted"));
/// ```
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A new process instance was started.
    ProcessStarted(payloads::ProcessStarted),
    /// A token arrived at a BPMN node.
    TokenArrived(payloads::TokenArrived),
    /// A token completed execution at a node.
    TokenCompleted(payloads::TokenCompleted),
    /// A user task was created and awaits human completion.
    UserTaskCreated(payloads::UserTaskCreated),
    /// A user task was completed by a human.
    UserTaskCompleted(payloads::UserTaskCompleted),
    /// A timer expired and is ready to fire.
    TimerFired(payloads::TimerFired),
    /// A timer was scheduled for future firing.
    TimerScheduled(payloads::TimerScheduled),
    /// A token failed during execution (triggers retry or compensation).
    TokenFailed(payloads::TokenFailed),
    /// A saga compensation sequence was started.
    SagaStarted(payloads::SagaStarted),
    /// A saga compensation sequence completed.
    SagaCompleted(payloads::SagaCompleted),
    /// All tokens in the process instance reached terminal states.
    ProcessCompleted(payloads::ProcessCompleted),
    /// A call activity invoked a child process.
    CallActivityStarted(payloads::CallActivityStarted),
    /// A call activity's child process completed.
    CallActivityCompleted(payloads::CallActivityCompleted),
    /// A named message was sent (intermediate throw event).
    MessageSent(payloads::MessageSent),
    /// A named signal was broadcast (signal intermediate throw event).
    SignalSent(payloads::SignalSent),
    /// The process instance was forcibly terminated.
    ProcessTerminated(payloads::ProcessTerminated),
    /// An external task was completed by a worker.
    ExternalTaskCompleted(payloads::ExternalTaskCompleted),
}

/// Reconstruct an [`EngineEvent`] from outbox columns (event_type + JSON payload).
///
/// Returns `None` if the event_type is unknown or the payload fails deserialization.
///
/// # Example
///
/// ```
/// use bpm_engine_core::{EngineEvent, event_from_outbox};
///
/// let payload = r#"{"process_id":"p1","instance_id":"i1"}"#;
/// let event = event_from_outbox("ProcessStarted", payload);
/// assert!(event.is_some());
///
/// // Unknown event type returns None
/// assert!(event_from_outbox("UnknownEvent", "{}").is_none());
///
/// // Invalid JSON returns None
/// assert!(event_from_outbox("ProcessStarted", "not-json").is_none());
/// ```
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
        "CallActivityStarted" => {
            EngineEvent::CallActivityStarted(serde_json::from_str(payload).ok()?)
        }
        "CallActivityCompleted" => {
            EngineEvent::CallActivityCompleted(serde_json::from_str(payload).ok()?)
        }
        "MessageSent" => EngineEvent::MessageSent(serde_json::from_str(payload).ok()?),
        "SignalSent" => EngineEvent::SignalSent(serde_json::from_str(payload).ok()?),
        "ProcessTerminated" => EngineEvent::ProcessTerminated(serde_json::from_str(payload).ok()?),
        "ExternalTaskCompleted" => {
            EngineEvent::ExternalTaskCompleted(serde_json::from_str(payload).ok()?)
        }
        _ => return None,
    };
    Some(ev)
}
