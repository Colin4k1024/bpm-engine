/// Unique identifier for a token within a process instance.
pub type TokenId = String;

/// Identifier for a group of tokens created by a parallel fork.
pub type ParallelGroupId = String;

/// Lifecycle state of a token. See [`crate::is_valid_token_transition`] for the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TokenStatus {
    /// Initial state after fork/start — not yet schedulable.
    Created,
    /// Schedulable — waiting for the engine to claim and execute.
    Ready,
    /// Actively being processed by a handler.
    Executing,
    /// Blocked on an external condition (timer, message, external task).
    Waiting,
    /// Paused by operator action; can be resumed to Ready.
    Suspended,
    /// Terminal: successfully completed execution at this node.
    Completed,
    /// Terminal: forcibly stopped (e.g., process cancellation or error).
    Terminated,
}

/// Execution direction of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TokenMode {
    /// Normal forward execution through the process graph.
    Forward,
    /// Reverse compensation execution (saga rollback).
    Compensation,
}

/// A token represents the authority to execute at a specific BPMN node.
///
/// Multiple tokens enable parallelism (fork/join). Each token progresses
/// independently through its lifecycle, and its state transitions are
/// persisted for crash recovery.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Token {
    pub id: TokenId,
    pub node_id: String,
    pub status: TokenStatus,
    pub mode: TokenMode,
    /// Optimistic concurrency version — incremented on each state change.
    pub version: u32,
    /// Retry attempt counter for failed execution.
    pub attempt: u32,
    /// Groups tokens created by the same parallel fork for join coordination.
    pub parallel_group_id: Option<ParallelGroupId>,
    pub updated_at: Option<String>,
}

impl Token {
    pub fn waiting(&self) -> bool {
        self.status == TokenStatus::Waiting
    }
}
