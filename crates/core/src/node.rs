use std::collections::HashMap;

pub type NodeId = &'static str;

#[derive(Debug, Clone)]
pub enum EdgeCondition {
    VariableEq { key: String, value: String },
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
    ServiceTask(fn(&mut super::instance::ProcessInstance)),
    UserTask,
    ExclusiveGateway,
    ParallelFork,
    ParallelJoin { expected: usize },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub node_type: NodeType,
    pub outgoing_edges: Vec<OutgoingEdge>,
}

#[derive(Debug, Clone)]
pub struct ProcessDefinition {
    pub id: &'static str,
    pub nodes: HashMap<NodeId, Node>,
    pub start: NodeId,
}
