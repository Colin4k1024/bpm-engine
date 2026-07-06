use std::collections::HashMap;

use super::token::Token;

/// Lifecycle state of a process instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum InstanceState {
    /// Process is active — at least one token has not reached a terminal state.
    Running,
    /// All tokens completed normally; end event reached.
    Completed,
    /// Process was forcibly cancelled or failed without recovery.
    Terminated,
}

/// Runtime container for a single execution of a BPMN process definition.
///
/// Holds the current set of tokens, process variables, and lifecycle state.
/// The engine persists instances after every state transition for crash safety.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessInstance {
    /// Unique instance identifier.
    pub id: String,
    /// Reference to the deployed process definition this instance executes.
    pub process_def_id: String,
    /// Optional tenant isolation key for multi-tenant deployments.
    pub tenant_id: Option<String>,
    /// Active and completed tokens belonging to this instance.
    pub tokens: Vec<Token>,
    /// Process-scoped variables (readable/writable by service tasks and gateways).
    pub variables: HashMap<String, String>,
    /// Current lifecycle state of the instance.
    pub state: InstanceState,
    /// Optimistic concurrency version for the instance record.
    pub version: u32,
    /// If this instance was started by a CallActivity, the parent instance ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_instance_id: Option<String>,
    /// If this instance was started by a CallActivity, the parent token ID to resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_token_id: Option<String>,
}

impl ProcessInstance {
    /// Returns `true` if the instance has reached the `Completed` state.
    pub fn completed(&self) -> bool {
        self.state == InstanceState::Completed
    }
}
