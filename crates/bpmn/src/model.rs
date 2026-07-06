//! BPMN AST: process, flow nodes, sequence flows (01.md).
//! Nodes carry incoming/outgoing as sequenceFlow IDs; wire_flows fills them.

use bpm_engine_core::FormField;
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
        form_fields: Option<Vec<FormField>>,
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
    SubProcess {
        id: String,
        name: Option<String>,
        flow_nodes: HashMap<String, BpmnFlowNode>,
        sequence_flows: Vec<BpmnSequenceFlow>,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    TimerIntermediateCatchEvent {
        id: String,
        name: Option<String>,
        timer_type: TimerType,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    BoundaryEvent {
        id: String,
        name: Option<String>,
        attached_to_ref: String,
        event_type: BoundaryEventType,
        is_interrupting: bool,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    CallActivity {
        id: String,
        name: Option<String>,
        called_element: String,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    MessageIntermediateCatchEvent {
        id: String,
        name: Option<String>,
        message_name: String,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    MessageIntermediateThrowEvent {
        id: String,
        name: Option<String>,
        message_name: String,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    SignalIntermediateThrowEvent {
        id: String,
        name: Option<String>,
        signal_name: String,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    SignalIntermediateCatchEvent {
        id: String,
        name: Option<String>,
        signal_name: String,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
    TerminateEndEvent {
        id: String,
        name: Option<String>,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    },
}

/// Type of boundary event.
#[derive(Debug, Clone)]
pub enum BoundaryEventType {
    Timer(TimerType),
    Error { error_code: Option<String> },
}

/// Timer definition types from BPMN timerEventDefinition.
#[derive(Debug, Clone)]
pub enum TimerType {
    /// ISO 8601 duration, e.g. "PT1H" (fire after 1 hour).
    TimeDuration(String),
    /// Absolute ISO 8601 datetime, e.g. "2025-01-01T00:00:00Z".
    TimeDate(String),
    /// ISO 8601 repeating interval, e.g. "R3/PT1H" (repeat 3 times every hour).
    TimeCycle(String),
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
            BpmnFlowNode::SubProcess { id, .. } => id,
            BpmnFlowNode::TimerIntermediateCatchEvent { id, .. } => id,
            BpmnFlowNode::BoundaryEvent { id, .. } => id,
            BpmnFlowNode::CallActivity { id, .. } => id,
            BpmnFlowNode::MessageIntermediateCatchEvent { id, .. } => id,
            BpmnFlowNode::MessageIntermediateThrowEvent { id, .. } => id,
            BpmnFlowNode::SignalIntermediateThrowEvent { id, .. } => id,
            BpmnFlowNode::SignalIntermediateCatchEvent { id, .. } => id,
            BpmnFlowNode::TerminateEndEvent { id, .. } => id,
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
            BpmnFlowNode::SubProcess { incoming, .. } => incoming,
            BpmnFlowNode::TimerIntermediateCatchEvent { incoming, .. } => incoming,
            BpmnFlowNode::BoundaryEvent { incoming, .. } => incoming,
            BpmnFlowNode::CallActivity { incoming, .. } => incoming,
            BpmnFlowNode::MessageIntermediateCatchEvent { incoming, .. } => incoming,
            BpmnFlowNode::MessageIntermediateThrowEvent { incoming, .. } => incoming,
            BpmnFlowNode::SignalIntermediateThrowEvent { incoming, .. } => incoming,
            BpmnFlowNode::SignalIntermediateCatchEvent { incoming, .. } => incoming,
            BpmnFlowNode::TerminateEndEvent { incoming, .. } => incoming,
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
            BpmnFlowNode::SubProcess { outgoing, .. } => outgoing,
            BpmnFlowNode::TimerIntermediateCatchEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::BoundaryEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::CallActivity { outgoing, .. } => outgoing,
            BpmnFlowNode::MessageIntermediateCatchEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::MessageIntermediateThrowEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::SignalIntermediateThrowEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::SignalIntermediateCatchEvent { outgoing, .. } => outgoing,
            BpmnFlowNode::TerminateEndEvent { outgoing, .. } => outgoing,
        }
    }
}

/// Trait for attaching flow IDs to nodes during wire_flows (01.md).
pub trait FlowAttach {
    fn add_incoming(&mut self, flow_id: &str);
    fn add_outgoing(&mut self, flow_id: &str);
    fn clear_flows(&mut self);
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
            BpmnFlowNode::SubProcess { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::TimerIntermediateCatchEvent { incoming, .. } => {
                incoming.push(flow_id.to_string())
            }
            BpmnFlowNode::BoundaryEvent { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::CallActivity { incoming, .. } => incoming.push(flow_id.to_string()),
            BpmnFlowNode::MessageIntermediateCatchEvent { incoming, .. } => {
                incoming.push(flow_id.to_string())
            }
            BpmnFlowNode::MessageIntermediateThrowEvent { incoming, .. } => {
                incoming.push(flow_id.to_string())
            }
            BpmnFlowNode::SignalIntermediateThrowEvent { incoming, .. } => {
                incoming.push(flow_id.to_string())
            }
            BpmnFlowNode::SignalIntermediateCatchEvent { incoming, .. } => {
                incoming.push(flow_id.to_string())
            }
            BpmnFlowNode::TerminateEndEvent { incoming, .. } => incoming.push(flow_id.to_string()),
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
            BpmnFlowNode::SubProcess { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::TimerIntermediateCatchEvent { outgoing, .. } => {
                outgoing.push(flow_id.to_string())
            }
            BpmnFlowNode::BoundaryEvent { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::CallActivity { outgoing, .. } => outgoing.push(flow_id.to_string()),
            BpmnFlowNode::MessageIntermediateCatchEvent { outgoing, .. } => {
                outgoing.push(flow_id.to_string())
            }
            BpmnFlowNode::MessageIntermediateThrowEvent { outgoing, .. } => {
                outgoing.push(flow_id.to_string())
            }
            BpmnFlowNode::SignalIntermediateThrowEvent { outgoing, .. } => {
                outgoing.push(flow_id.to_string())
            }
            BpmnFlowNode::SignalIntermediateCatchEvent { outgoing, .. } => {
                outgoing.push(flow_id.to_string())
            }
            BpmnFlowNode::TerminateEndEvent { outgoing, .. } => outgoing.push(flow_id.to_string()),
        }
    }
    fn clear_flows(&mut self) {
        match self {
            BpmnFlowNode::StartEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::EndEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::ServiceTask {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::UserTask {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::ExclusiveGateway {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::ParallelGateway {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::SubProcess {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::TimerIntermediateCatchEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::BoundaryEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::CallActivity {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::MessageIntermediateCatchEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::MessageIntermediateThrowEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::SignalIntermediateThrowEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::SignalIntermediateCatchEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
            BpmnFlowNode::TerminateEndEvent {
                incoming, outgoing, ..
            } => {
                incoming.clear();
                outgoing.clear();
            }
        }
    }
}
