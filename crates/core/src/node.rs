use std::collections::HashMap;

/// Node identifier within a process definition (static lifetime from BPMN compilation).
pub type NodeId = &'static str;

/// Condition evaluated at exclusive gateways to select the outgoing path.
#[derive(Debug, Clone)]
pub enum EdgeCondition {
    /// Exact variable match: `variables[key] == value`.
    VariableEq { key: String, value: String },
    /// Free-form expression (evaluated by the EL engine).
    Expression(String),
    /// Default path taken when no other condition matches.
    Default,
}

/// A directed edge from one BPMN node to another, optionally guarded by a condition.
#[derive(Debug, Clone)]
pub struct OutgoingEdge {
    pub target: NodeId,
    pub condition: Option<EdgeCondition>,
}

/// The semantic type of a BPMN node, determining how the engine executes it.
#[derive(Debug, Clone)]
pub enum NodeType {
    /// BPMN Start Event — creates the initial token.
    Start,
    /// BPMN End Event — consumes and completes the arriving token.
    End,
    /// Inline service task: synchronous function executed in-process.
    ServiceTask(fn(&mut super::instance::ProcessInstance)),
    /// Human task — token waits until external completion signal.
    UserTask,
    /// Pull-based external task: worker fetches, locks, completes/fails.
    ExternalTask {
        task_type: String,
        retries: i32,
        timeout_secs: u64,
    },
    /// XOR gateway — routes to exactly one outgoing edge based on conditions.
    ExclusiveGateway,
    /// AND split — creates one token per outgoing edge.
    ParallelFork,
    /// AND join — waits until all expected branches arrive before continuing.
    ParallelJoin { expected: usize },
}

/// A node in the process graph with its type and outgoing connections.
#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub node_type: NodeType,
    pub outgoing_edges: Vec<OutgoingEdge>,
}

/// Compiled BPMN process definition ready for execution.
///
/// Created by [`bpm_engine_bpmn::parse_and_compile`] from BPMN 2.0 XML.
/// Contains the full node graph with static-lifetime string references
/// (allocated via `Box::leak` during compilation for zero-copy execution).
#[derive(Debug, Clone)]
pub struct ProcessDefinition {
    pub id: &'static str,
    pub nodes: HashMap<NodeId, Node>,
    /// Entry point node for new process instances.
    pub start: NodeId,
}
