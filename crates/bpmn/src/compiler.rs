//! BPMN model → Engine ProcessDefinition compiler (03.md).
//! Collects all CompilerErrors; check_* steps; build_outgoing from node.outgoing + flows.

use bpm_engine_core::{EdgeCondition, Node, NodeType, OutgoingEdge, ProcessDefinition};
use std::collections::{HashMap, HashSet};

use crate::errors::{CompilerError, ErrorCode};
use crate::model::{BpmnFlowNode, BpmnProcess, BpmnSequenceFlow};

/// Map node id -> (NodeType, outgoing edges as (target, condition)).
type NodeOutgoingMap = HashMap<String, (NodeType, Vec<(String, Option<EdgeCondition>)>)>;

/// Compile BPMN process to engine ProcessDefinition. Returns all errors (no fail-fast).
pub fn compile(model: BpmnProcess) -> Result<ProcessDefinition, Vec<CompilerError>> {
    let mut errors = Vec::new();

    check_start_end(&model, &mut errors);
    check_sequence_flows(&model, &mut errors);
    check_orphan_nodes(&model, &mut errors);
    check_gateways(&model, &mut errors);
    check_dead_end(&model, &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }

    let outgoing = build_outgoing(&model);
    let nodes = build_nodes(&model, &outgoing);
    to_engine_definition(&model.id, &model, nodes)
}

fn err(
    code: ErrorCode,
    message: impl Into<String>,
    node_id: Option<String>,
    flow_id: Option<String>,
    hint: Option<String>,
) -> CompilerError {
    CompilerError {
        code,
        message: message.into(),
        node_id,
        flow_id,
        hint,
    }
}

fn check_start_end(model: &BpmnProcess, errors: &mut Vec<CompilerError>) {
    let starts: Vec<_> = model
        .flow_nodes
        .values()
        .filter(|n| matches!(n, BpmnFlowNode::StartEvent { .. }))
        .collect();
    if starts.is_empty() {
        errors.push(err(
            ErrorCode::NoStartEvent,
            "Process must contain exactly one startEvent",
            None,
            None,
            None,
        ));
    } else if starts.len() > 1 {
        for s in &starts {
            errors.push(err(
                ErrorCode::MultipleStartEvents,
                "Process must contain exactly one startEvent",
                Some(s.id().to_string()),
                None,
                None,
            ));
        }
    }

    let ends: Vec<_> = model
        .flow_nodes
        .values()
        .filter(|n| matches!(n, BpmnFlowNode::EndEvent { .. }))
        .collect();
    if ends.is_empty() {
        errors.push(err(
            ErrorCode::NoEndEvent,
            "Process must contain at least one endEvent",
            None,
            None,
            None,
        ));
    }
}

fn check_sequence_flows(model: &BpmnProcess, errors: &mut Vec<CompilerError>) {
    for flow in &model.sequence_flows {
        if !model.flow_nodes.contains_key(&flow.source_ref) {
            errors.push(err(
                ErrorCode::SequenceFlowSourceNotFound,
                format!("Flow {} sourceRef {} not found", flow.id, flow.source_ref),
                None,
                Some(flow.id.clone()),
                None,
            ));
        }
        if !model.flow_nodes.contains_key(&flow.target_ref) {
            errors.push(err(
                ErrorCode::SequenceFlowTargetNotFound,
                format!("Flow {} targetRef {} not found", flow.id, flow.target_ref),
                None,
                Some(flow.id.clone()),
                None,
            ));
        }
        if flow.source_ref == flow.target_ref {
            errors.push(err(
                ErrorCode::SequenceFlowSourceNotFound,
                "Sequence flow self-loop not supported",
                Some(flow.source_ref.clone()),
                Some(flow.id.clone()),
                None,
            ));
        }
    }
}

fn check_orphan_nodes(model: &BpmnProcess, errors: &mut Vec<CompilerError>) {
    let start_id = model
        .flow_nodes
        .values()
        .find(|n| matches!(n, BpmnFlowNode::StartEvent { .. }))
        .map(|n| n.id().to_string());
    let end_ids: HashSet<String> = model
        .flow_nodes
        .values()
        .filter(|n| matches!(n, BpmnFlowNode::EndEvent { .. }))
        .map(|n| n.id().to_string())
        .collect();

    for (id, node) in &model.flow_nodes {
        if start_id.as_deref() == Some(id.as_str()) {
            continue;
        }
        if node.incoming().is_empty() {
            errors.push(err(
                ErrorCode::OrphanNode,
                format!("Node {} has no incoming sequence flow", id),
                Some(id.clone()),
                None,
                Some("Did you forget to connect it?".to_string()),
            ));
        }
    }

    for (id, node) in &model.flow_nodes {
        if end_ids.contains(id) {
            continue;
        }
        if node.outgoing().is_empty() {
            errors.push(err(
                ErrorCode::OrphanNode,
                format!("Node {} has no outgoing sequence flow", id),
                Some(id.clone()),
                None,
                Some("Did you forget to connect it?".to_string()),
            ));
        }
    }
}

fn check_gateways(model: &BpmnProcess, errors: &mut Vec<CompilerError>) {
    let flows_by_id: HashMap<&str, &BpmnSequenceFlow> = model
        .sequence_flows
        .iter()
        .map(|f| (f.id.as_str(), f))
        .collect();

    for (id, node) in &model.flow_nodes {
        if let BpmnFlowNode::ExclusiveGateway { .. } = node {
            let outgoing = node.outgoing();
            let default_count = outgoing
                .iter()
                .filter(|fid| {
                    flows_by_id
                        .get(fid.as_str())
                        .map(|f| f.is_default)
                        .unwrap_or(false)
                })
                .count();
            if default_count > 1 {
                errors.push(err(
                    ErrorCode::ExclusiveGatewayNoDefault,
                    "Exclusive gateway must have at most one default flow",
                    Some(id.clone()),
                    None,
                    None,
                ));
            }
            let with_condition = outgoing
                .iter()
                .filter(|fid| {
                    flows_by_id
                        .get(fid.as_str())
                        .map(|f| f.condition_expression.is_some() || f.is_default)
                        .unwrap_or(false)
                })
                .count();
            if default_count == 0 && with_condition < outgoing.len() {
                errors.push(err(
                    ErrorCode::ExclusiveGatewayNoDefault,
                    "Gateway has conditional flows but no default",
                    Some(id.clone()),
                    None,
                    Some("Mark one sequenceFlow as default".to_string()),
                ));
            }
        }

        if let BpmnFlowNode::ParallelGateway { .. } = node {
            let inc = node.incoming().len();
            let out = node.outgoing().len();
            if inc != 1 && out != 1 {
                errors.push(err(
                    ErrorCode::ParallelGatewayInvalidShape,
                    format!(
                        "Gateway has {} incoming and {} outgoing; cannot act as both fork and join",
                        inc, out
                    ),
                    Some(id.clone()),
                    None,
                    Some("Split into separate fork and join gateways".to_string()),
                ));
            }
            if inc == 1 && out < 2 {
                errors.push(err(
                    ErrorCode::ParallelGatewayInvalidShape,
                    "Parallel fork must have at least 2 outgoing flows",
                    Some(id.clone()),
                    None,
                    None,
                ));
            }
            if inc < 2 && out == 1 && inc != 1 {
                errors.push(err(
                    ErrorCode::ParallelGatewayInvalidShape,
                    "Parallel join must have at least 2 incoming flows",
                    Some(id.clone()),
                    None,
                    None,
                ));
            }
        }
    }
}

fn check_dead_end(model: &BpmnProcess, errors: &mut Vec<CompilerError>) {
    let start_id = match model
        .flow_nodes
        .values()
        .find(|n| matches!(n, BpmnFlowNode::StartEvent { .. }))
    {
        Some(n) => n.id().to_string(),
        None => return,
    };
    let end_ids: HashSet<String> = model
        .flow_nodes
        .values()
        .filter(|n| matches!(n, BpmnFlowNode::EndEvent { .. }))
        .map(|n| n.id().to_string())
        .collect();
    let flow_by_id: HashMap<&str, &BpmnSequenceFlow> = model
        .sequence_flows
        .iter()
        .map(|f| (f.id.as_str(), f))
        .collect();

    let mut reachable: HashSet<String> = HashSet::new();
    let mut stack = vec![start_id.clone()];
    while let Some(nid) = stack.pop() {
        if !reachable.insert(nid.clone()) {
            continue;
        }
        let node = match model.flow_nodes.get(&nid) {
            Some(n) => n,
            None => continue,
        };
        if matches!(node, BpmnFlowNode::EndEvent { .. }) {
            continue;
        }
        for fid in node.outgoing() {
            if let Some(flow) = flow_by_id.get(fid.as_str()) {
                stack.push(flow.target_ref.clone());
            }
        }
    }

    let mut can_reach_end: HashSet<String> = end_ids.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for flow in &model.sequence_flows {
            if can_reach_end.contains(&flow.target_ref)
                && reachable.contains(&flow.source_ref)
                && can_reach_end.insert(flow.source_ref.clone())
            {
                changed = true;
            }
        }
    }

    for nid in &reachable {
        if end_ids.contains(nid) {
            continue;
        }
        if !can_reach_end.contains(nid) {
            errors.push(err(
                ErrorCode::DeadEnd,
                format!("Node {} leads to no end event", nid),
                Some(nid.clone()),
                None,
                None,
            ));
        }
    }
}

/// Outgoing edges per node id: (target, condition). Built from node.outgoing + flows (01.md).
fn build_outgoing(model: &BpmnProcess) -> HashMap<String, Vec<(String, Option<EdgeCondition>)>> {
    let flow_by_id: HashMap<&str, &BpmnSequenceFlow> = model
        .sequence_flows
        .iter()
        .map(|f| (f.id.as_str(), f))
        .collect();

    let mut result: HashMap<String, Vec<(String, Option<EdgeCondition>)>> = HashMap::new();
    for (id, node) in &model.flow_nodes {
        if matches!(node, BpmnFlowNode::EndEvent { .. }) {
            continue;
        }
        let edges: Vec<(String, Option<EdgeCondition>)> = node
            .outgoing()
            .iter()
            .filter_map(|fid| {
                let flow = flow_by_id.get(fid.as_str())?;
                let cond = flow
                    .condition_expression
                    .as_ref()
                    .map(|raw| parse_condition(raw, flow.is_default));
                Some((flow.target_ref.clone(), cond))
            })
            .collect();
        result.insert(id.clone(), edges);
    }
    result
}

fn parse_condition(raw: &str, is_default: bool) -> EdgeCondition {
    if is_default {
        return EdgeCondition::Default;
    }
    let s = raw.trim();
    let inner = s
        .strip_prefix("${")
        .and_then(|t| t.strip_suffix('}'))
        .unwrap_or(s)
        .trim();
    if inner.is_empty() {
        EdgeCondition::Default
    } else {
        EdgeCondition::Expression(inner.to_string())
    }
}

fn build_nodes(
    model: &BpmnProcess,
    outgoing: &HashMap<String, Vec<(String, Option<EdgeCondition>)>>,
) -> NodeOutgoingMap {
    let mut incoming_count: HashMap<String, usize> = HashMap::new();
    for flow in &model.sequence_flows {
        *incoming_count.entry(flow.target_ref.clone()).or_insert(0) += 1;
    }

    let mut result: NodeOutgoingMap = HashMap::new();
    for (id, node) in &model.flow_nodes {
        let out = outgoing.get(id).cloned().unwrap_or_default();
        let node_type = match node {
            BpmnFlowNode::StartEvent { .. } => NodeType::Start,
            BpmnFlowNode::EndEvent { .. } => NodeType::End,
            BpmnFlowNode::ServiceTask {
                task_type,
                retries,
                timeout_secs,
                ..
            } => NodeType::ExternalTask {
                task_type: task_type.clone(),
                retries: *retries,
                timeout_secs: *timeout_secs,
            },
            BpmnFlowNode::UserTask { .. } => NodeType::UserTask,
            BpmnFlowNode::ExclusiveGateway { .. } => NodeType::ExclusiveGateway,
            BpmnFlowNode::ParallelGateway { .. } => {
                let inc = incoming_count.get(id).copied().unwrap_or(0);
                if inc != 1 {
                    NodeType::ParallelJoin { expected: inc }
                } else {
                    NodeType::ParallelFork
                }
            }
        };
        result.insert(id.clone(), (node_type, out));
    }
    result
}

fn to_engine_definition(
    process_id: &str,
    model: &BpmnProcess,
    nodes: NodeOutgoingMap,
) -> Result<ProcessDefinition, Vec<CompilerError>> {
    let start_id = model
        .flow_nodes
        .values()
        .find(|n| matches!(n, BpmnFlowNode::StartEvent { .. }))
        .map(|n| n.id().to_string())
        .expect("validated");

    let mut all_strings: HashSet<String> = HashSet::new();
    all_strings.insert(process_id.to_string());
    all_strings.insert(start_id.clone());
    for (id, (_, out)) in &nodes {
        all_strings.insert(id.clone());
        for (target, _) in out {
            all_strings.insert(target.clone());
        }
    }

    let leaked: HashMap<String, &'static str> = all_strings
        .into_iter()
        .map(|s| {
            let static_ref: &'static str = Box::leak(s.into_boxed_str());
            (static_ref.to_string(), static_ref)
        })
        .collect();
    let get = |s: &str| *leaked.get(s).expect("string was collected");

    let mut engine_nodes: HashMap<&'static str, Node> = HashMap::new();
    for (id, (node_type, out_edges)) in nodes {
        let edges: Vec<OutgoingEdge> = out_edges
            .into_iter()
            .map(|(target, cond)| OutgoingEdge {
                target: get(&target),
                condition: cond,
            })
            .collect();
        engine_nodes.insert(
            get(&id),
            Node {
                id: get(&id),
                node_type,
                outgoing_edges: edges,
            },
        );
    }

    Ok(ProcessDefinition {
        id: get(process_id),
        start: get(&start_id),
        nodes: engine_nodes,
    })
}
