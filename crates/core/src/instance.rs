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
    pub id: String,
    /// Reference to the deployed process definition this instance executes.
    pub process_def_id: String,
    /// Optional tenant isolation key for multi-tenant deployments.
    pub tenant_id: Option<String>,
    /// Active and completed tokens belonging to this instance.
    pub tokens: Vec<Token>,
    /// Process-scoped variables (readable/writable by service tasks and gateways).
    pub variables: HashMap<String, String>,
    pub state: InstanceState,
    /// Optimistic concurrency version for the instance record.
    pub version: u32,
}

impl ProcessInstance {
    pub fn completed(&self) -> bool {
        self.state == InstanceState::Completed
    }
}
