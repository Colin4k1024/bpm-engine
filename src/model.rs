use std::collections::HashMap;

pub type NodeId = &'static str;

/// Process instance lifecycle state (design: overview §2.2, whitepaper §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    Running,
    Completed,
    Terminated,
}

/// Token lifecycle states (design: core §2, whitepaper §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStatus {
    Created,
    Ready,
    Executing,
    Waiting,
    Suspended,
    Completed,
    Terminated,
}

/// Token execution mode (design: saga, whitepaper §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenMode {
    Forward,
    Compensation,
}

/// Condition on an outgoing edge of an Exclusive Gateway.
#[derive(Debug, Clone)]
pub enum EdgeCondition {
    VariableEq { key: String, value: String },
    /// EL expression: evaluated against instance variables; first matching edge wins.
    Expression(String),
    Default,
}

#[derive(Debug, Clone)]
pub struct OutgoingEdge {
    pub target: NodeId,
    pub condition: Option<EdgeCondition>,
}

#[derive(Debug, Clone)]
pub enum NodeType {
    Start,
    End,
    ServiceTask(fn(&mut ProcessInstance)),
    UserTask,
    ExclusiveGateway,
    /// One token in, N tokens out (design: core.md §5.1).
    ParallelFork,
    /// N tokens in, one out; expected = number of incoming branches (design: core.md §5.2).
    ParallelJoin { expected: usize },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub node_type: NodeType,
    /// For non-gateway nodes, condition is always None. For ExclusiveGateway, first matching edge wins (Default = fallback).
    pub outgoing_edges: Vec<OutgoingEdge>,
}

#[derive(Debug, Clone)]
pub struct ProcessDefinition {
    pub id: &'static str,
    pub nodes: HashMap<NodeId, Node>,
    pub start: NodeId,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub id: String,
    pub node_id: String,
    pub status: TokenStatus,
    pub mode: TokenMode,
    /// Optimistic lock (whitepaper §11.3).
    pub version: u32,
    /// Retry/attempt counter (docs_database_schema §3; recovery).
    pub attempt: u32,
    /// For Parallel Fork/Join (design: core.md §5).
    pub parallel_group_id: Option<String>,
    /// Timestamp of last update (recovery, whitepaper §12).
    pub updated_at: Option<String>,
}

impl Token {
    /// Backward compat: waiting iff status == Waiting.
    pub fn waiting(&self) -> bool {
        self.status == TokenStatus::Waiting
    }
}

#[derive(Debug)]
pub struct ProcessInstance {
    pub id: String,
    pub process_def_id: String,
    pub tokens: Vec<Token>,
    pub variables: HashMap<String, String>,
    /// Instance lifecycle (whitepaper §4). Persisted as state; completed derived.
    pub state: InstanceState,
    /// Optimistic lock (docs_database_schema §2).
    pub version: u32,
}

impl ProcessInstance {
    /// True when state is Completed (backward compat).
    pub fn completed(&self) -> bool {
        self.state == InstanceState::Completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// docs_testing_strategy §3.1: Token waiting() is derived from status.
    #[test]
    fn token_waiting_only_when_waiting_status() {
        let t_waiting = Token {
            id: "1".into(),
            node_id: "n".into(),
            status: TokenStatus::Waiting,
            mode: TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        };
        assert!(t_waiting.waiting());
        let t_ready = Token {
            id: "2".into(),
            node_id: "n".into(),
            status: TokenStatus::Ready,
            mode: TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        };
        assert!(!t_ready.waiting());
    }

    /// docs_testing_strategy §3.1: Instance completed() is derived from state.
    #[test]
    fn instance_completed_only_when_completed_state() {
        let inst_running = ProcessInstance {
            id: "i".into(),
            process_def_id: "p".into(),
            tokens: vec![],
            variables: HashMap::new(),
            state: InstanceState::Running,
            version: 0,
        };
        assert!(!inst_running.completed());
        let inst_done = ProcessInstance {
            id: "i".into(),
            process_def_id: "p".into(),
            tokens: vec![],
            variables: HashMap::new(),
            state: InstanceState::Completed,
            version: 0,
        };
        assert!(inst_done.completed());
    }
}
