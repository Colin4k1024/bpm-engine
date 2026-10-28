//! Lowering: [`DslDocument`] -> [`bpm_engine_bpmn`] intermediate model.
//!
//! Every field maps field-by-field onto what the BPMN parser would produce for
//! an equivalent document (including defaults: `retries = 3`,
//! `timeout_secs = 60`, form-field label defaulting to the field id), so the
//! shared compiler applies identical validation and produces identical
//! output (ADR-006).

use std::collections::HashMap;

use bpm_engine_bpmn::model::{BpmnFlowNode, BpmnProcess, BpmnSequenceFlow, TimerType};
use bpm_engine_bpmn::parser::wire_flows;
use bpm_engine_core::{FormField, FormFieldType};

use crate::types::{
    DslCondition, DslDocument, DslEdge, DslFormField, DslFormFieldType, DslNode, DslNodeType,
};

/// Default `retries` for service tasks — matches the BPMN parser default.
const DEFAULT_SERVICE_RETRIES: i32 = 3;
/// Default `timeout_secs` for service tasks — matches the BPMN parser value.
const DEFAULT_SERVICE_TIMEOUT_SECS: u64 = 60;

/// Resolve the effective edge id: the user-provided id, or a deterministic
/// generated one (`edge_<index>`). Shared by validation (duplicate detection)
/// and lowering so both agree on edge identity.
pub(crate) fn resolved_edge_id(edge: &DslEdge, index: usize) -> String {
    edge.id.clone().unwrap_or_else(|| format!("edge_{}", index))
}

/// Lower a validated document into the shared `BpmnProcess` intermediate
/// model (with incoming/outgoing wired exactly as the BPMN parser does).
pub(crate) fn lower(doc: &DslDocument) -> BpmnProcess {
    let mut flow_nodes: HashMap<String, BpmnFlowNode> = HashMap::new();
    for node in &doc.nodes {
        let built = lower_node(node);
        flow_nodes.insert(node.id.clone(), built);
    }

    let sequence_flows: Vec<BpmnSequenceFlow> = doc
        .edges
        .iter()
        .enumerate()
        .map(|(i, edge)| BpmnSequenceFlow {
            id: resolved_edge_id(edge, i),
            source_ref: edge.source.clone(),
            target_ref: edge.target.clone(),
            condition_expression: edge.condition.as_ref().map(lower_condition),
            is_default: edge.default.unwrap_or(false),
        })
        .collect();

    let mut model = BpmnProcess {
        id: doc.process.id.clone(),
        name: doc.process.name.clone(),
        flow_nodes,
        sequence_flows,
    };
    // Identical to the parser's final step: attach flow ids to nodes.
    wire_flows(&mut model);
    model
}

/// Lower a single node, reproducing the BPMN parser's field mapping.
fn lower_node(node: &DslNode) -> BpmnFlowNode {
    let id = node.id.clone();
    let name = node.name.clone();
    match node.node_type {
        DslNodeType::Start => BpmnFlowNode::StartEvent {
            id,
            name,
            incoming: vec![],
            outgoing: vec![],
        },
        DslNodeType::End => BpmnFlowNode::EndEvent {
            id,
            name,
            incoming: vec![],
            outgoing: vec![],
        },
        DslNodeType::ServiceTask => BpmnFlowNode::ServiceTask {
            id,
            name,
            // Validation makes task_type required for serviceTask; the
            // "default" fallback mirrors the BPMN parser for safety.
            task_type: node
                .task_type
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            retries: node.retries.unwrap_or(DEFAULT_SERVICE_RETRIES),
            timeout_secs: node.timeout_secs.unwrap_or(DEFAULT_SERVICE_TIMEOUT_SECS),
            incoming: vec![],
            outgoing: vec![],
        },
        DslNodeType::UserTask => BpmnFlowNode::UserTask {
            id,
            name,
            form_key: node.form_key.clone(),
            form_fields: node
                .form_fields
                .as_ref()
                .map(|fields| fields.iter().map(lower_form_field).collect()),
            incoming: vec![],
            outgoing: vec![],
        },
        DslNodeType::ExclusiveGateway => BpmnFlowNode::ExclusiveGateway {
            id,
            name,
            incoming: vec![],
            outgoing: vec![],
        },
        DslNodeType::ParallelGateway => BpmnFlowNode::ParallelGateway {
            id,
            name,
            incoming: vec![],
            outgoing: vec![],
        },
        DslNodeType::TimerIntermediateCatch => BpmnFlowNode::TimerIntermediateCatchEvent {
            id,
            name,
            // Validation makes timer required for timer nodes.
            timer_type: TimerType::TimeDuration(
                node.timer
                    .as_ref()
                    .map(|t| t.duration.clone())
                    .unwrap_or_default(),
            ),
            incoming: vec![],
            outgoing: vec![],
        },
    }
}

/// Lower an edge condition to the raw `condition_expression` string the BPMN
/// parser would store.
///
/// `variableEq` lowers to the canonical expression `${variable == "value"}`,
/// which the shared compiler turns into `EdgeCondition::Expression`; an
/// `expression` condition passes through verbatim (the shared compiler strips
/// an optional `${...}` wrapper, same as for BPMN `conditionExpression` text).
fn lower_condition(condition: &DslCondition) -> String {
    match condition {
        DslCondition::VariableEq { variable, value } => {
            format!("${{{} == \"{}\"}}", variable, value)
        }
        DslCondition::Expression { expression } => expression.clone(),
    }
}

/// Lower a form field, reproducing the BPMN parser's defaults (label defaults
/// to the field id, type defaults to string, empty options become `None`).
fn lower_form_field(field: &DslFormField) -> FormField {
    let field_type = match field.field_type.unwrap_or(DslFormFieldType::String) {
        DslFormFieldType::String => FormFieldType::String,
        DslFormFieldType::Number => FormFieldType::Number,
        DslFormFieldType::Boolean => FormFieldType::Boolean,
        DslFormFieldType::Enum => FormFieldType::Enum,
    };
    FormField {
        id: field.id.clone(),
        label: field.label.clone().unwrap_or_else(|| field.id.clone()),
        field_type,
        required: field.required.unwrap_or(false),
        default_value: field.default_value.clone(),
        options: match &field.options {
            Some(options) if !options.is_empty() => Some(options.clone()),
            _ => None,
        },
    }
}
