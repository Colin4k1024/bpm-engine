//! BPMN AST: process, flow nodes, sequence flows (01.md).
//! Nodes carry incoming/outgoing as sequenceFlow IDs; wire_flows fills them.

use std::collections::HashMap;

/// BPMN process (one per definitions for MVP).
#[derive(Debug, Clone)]
pub struct BpmnProcess {
    pub id: String,
    pub name: Option<String>,
    pub flow_nodes: HashMap<String, BpmnFlowNode>,
    pub sequence_flows: Vec<BpmnSequenceFlow>,
}

#[derive(Debug, Clone)]
pub struct BpmnSequenceFlow {
    pub id: String,
    pub source_ref: String,
    pub target_ref: String,
    /// Raw condition expression, e.g. "${var == \"xxx\"}".
    pub condition_expression: Option<String>,
    /// BPMN default flow from gateway.
    pub is_default: bool,
}

#[derive(Debug, Clone)]
pub enum BpmnFlowNode {
    StartEvent {
        id: String,
        name: Option<String>,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    EndEvent {
        id: String,
        name: Option<String>,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    ServiceTask {
        id: String,
        name: Option<String>,
        task_type: String,
        retries: i32,
        timeout_secs: u64,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    UserTask {
        id: String,
        name: Option<String>,
        form_key: Option<String>,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    ExclusiveGateway {
        id: String,
        name: Option<String>,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    ParallelGateway {
        id: String,
        name: Option<String>,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
}

impl BpmnFlowNode {
    pub fn id(&self) -> &str {
        match self {
            BpmnFlowNode::StartEvent { id, .. } => id,
            BpmnFlowNode::EndEvent { id, .. } => id,
            BpmnFlowNode::ServiceTask { id, .. } => id,
            BpmnFlowNode::UserTask { id, .. } => id,
            BpmnFlowNode::ExclusiveGateway { id, .. } => id,
            BpmnFlowNode::ParallelGateway { id, .. } => id,
        }
    }

    pub fn incoming(&self) -> &[String] {
        match self {
            BpmnFlowNode::StartEvent { incoming, .. } => incoming,
            BpmnFlowNode::EndEvent { incoming, .. } => incoming,
            BpmnFlowNode::ServiceTask { incoming, .. } => incoming,
            BpmnFlowNode::UserTask { incoming, .. } => incoming,
            BpmnFlowNode::ExclusiveGateway { incoming, .. } => incoming,
            BpmnFlowNode::ParallelGateway { incoming, .. } => incoming,
        }
    }

    pub fn outgoing(&self) -> &[String] {
        match self {
            BpmnFlowNode::StartEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::EndEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::ServiceTask { outgoing, .. } => outgoing,
            BpmnFlowNode::UserTask { outgoing, .. } => outgoing,
            BpmnFlowNode::ExclusiveGateway { outgoing, .. } => outgoing,
            BpmnFlowNode::ParallelGateway { outgoing, .. } => outgoing,
        }
    }
}

/// Trait for attaching flow IDs to nodes during wire_flows (01.md).
pub trait FlowAttach {
    fn add_incoming(&mut self, flow_id: &str);
    fn add_outgoing(&mut self, flow_id: &str);
}

impl FlowAttach for BpmnFlowNode {
    fn add_incoming(&mut self, flow_id: &str) {
        match self {
            BpmnFlowNode::StartEvent { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::EndEvent { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::ServiceTask { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::UserTask { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::ExclusiveGateway { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::ParallelGateway { incoming, .. } => incoming.push(flow_id.to_string()),
        }
    }
    fn add_outgoing(&mut self, flow_id: &str) {
        match self {
            BpmnFlowNode::StartEvent { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::EndEvent { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::ServiceTask { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::UserTask { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::ExclusiveGateway { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::ParallelGateway { outgoing, .. } => outgoing.push(flow_id.to_string()),
        }
    }
}
