//! BPMN model → Engine ProcessDefinition compiler (03.md).
//! Collects all CompilerErrors; check_* steps; build_outgoing from node.outgoing + flows.

use bpm_engine_core::{
    BoundaryEventDef, EdgeCondition, Node, NodeType, OutgoingEdge, ProcessDefinition,
};
use std::collections::{HashMap, HashSet};

use crate::errors::{CompilerError, ErrorCode};
use crate::model::{BoundaryEventType, BpmnFlowNode, BpmnProcess, BpmnSequenceFlow, TimerType};

/// Map node id -> (NodeType, outgoing edges as (target, condition)).
type NodeOutgoingMap = HashMap<String, (NodeType, Vec<(String, Option<EdgeCondition>)>)>;

/// Compile BPMN process to engine ProcessDefinition. Returns all errors (no fail-fast).
pub fn compile(model: BpmnProcess) -> Result<ProcessDefinition, Vec<CompilerError>> {
    let mut errors = Vec::new();

    check_subprocesses(&model, &mut errors);
    check_start_end(&model, &mut errors);
    check_sequence_flows(&model, &mut errors);
    check_orphan_nodes(&model, &mut errors);
    check_gateways(&model, &mut errors);
    check_dead_end(&model, &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }

    // Flatten subProcesses: promote internal nodes to parent scope
    let mut flat_model = flatten_subprocesses(model);
    // Re-wire incoming/outgoing flow IDs on flattened nodes
    crate::parser::wire_flows(&mut flat_model);

    let outgoing = build_outgoing(&flat_model);
    let nodes = build_nodes(&flat_model, &outgoing);
    to_engine_definition(&flat_model.id, &flat_model, nodes)
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
        .filter(|n| {
            matches!(
                n,
                BpmnFlowNode::EndEvent { .. } | BpmnFlowNode::TerminateEndEvent { .. }
            )
        })
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
        .filter(|n| {
            matches!(
                n,
                BpmnFlowNode::EndEvent { .. } | BpmnFlowNode::TerminateEndEvent { .. }
            )
        })
        .map(|n| n.id().to_string())
        .collect();

    for (id, node) in &model.flow_nodes {
        if start_id.as_deref() == Some(id.as_str()) {
            continue;
        }
        // Boundary events are triggered by their host, not by incoming flows
        if matches!(node, BpmnFlowNode::BoundaryEvent { .. }) {
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
        .filter(|n| {
            matches!(
                n,
                BpmnFlowNode::EndEvent { .. } | BpmnFlowNode::TerminateEndEvent { .. }
            )
        })
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

/// Validate subProcess elements have exactly one start and one end event.
fn check_subprocesses(model: &BpmnProcess, errors: &mut Vec<CompilerError>) {
    for (id, node) in &model.flow_nodes {
        if let BpmnFlowNode::SubProcess {
            flow_nodes,
            sequence_flows,
            ..
        } = node
        {
            let starts: Vec<_> = flow_nodes
                .values()
                .filter(|n| matches!(n, BpmnFlowNode::StartEvent { .. }))
                .collect();
            if starts.is_empty() {
                errors.push(err(
                    ErrorCode::NoStartEvent,
                    format!("SubProcess {} must contain a startEvent", id),
                    Some(id.clone()),
                    None,
                    None,
                ));
            } else if starts.len() > 1 {
                errors.push(err(
                    ErrorCode::MultipleStartEvents,
                    format!("SubProcess {} must contain exactly one startEvent", id),
                    Some(id.clone()),
                    None,
                    None,
                ));
            }

            let ends: Vec<_> = flow_nodes
                .values()
                .filter(|n| matches!(n, BpmnFlowNode::EndEvent { .. }))
                .collect();
            if ends.is_empty() {
                errors.push(err(
                    ErrorCode::NoEndEvent,
                    format!("SubProcess {} must contain an endEvent", id),
                    Some(id.clone()),
                    None,
                    None,
                ));
            }

            // Validate sequence flow references within subprocess
            for flow in sequence_flows {
                if !flow_nodes.contains_key(&flow.source_ref) {
                    errors.push(err(
                        ErrorCode::SequenceFlowSourceNotFound,
                        format!(
                            "SubProcess {} flow {} sourceRef {} not found",
                            id, flow.id, flow.source_ref
                        ),
                        Some(id.clone()),
                        Some(flow.id.clone()),
                        None,
                    ));
                }
                if !flow_nodes.contains_key(&flow.target_ref) {
                    errors.push(err(
                        ErrorCode::SequenceFlowTargetNotFound,
                        format!(
                            "SubProcess {} flow {} targetRef {} not found",
                            id, flow.id, flow.target_ref
                        ),
                        Some(id.clone()),
                        Some(flow.id.clone()),
                        None,
                    ));
                }
            }

            // Recursively check nested subProcesses
            let sub_model = BpmnProcess {
                id: format!("{}-sub", model.id),
                name: None,
                flow_nodes: flow_nodes.clone(),
                sequence_flows: sequence_flows.clone(),
            };
            check_subprocesses(&sub_model, errors);
        }
    }
}

/// Flatten all subProcess nodes: promote internal nodes to parent scope with
/// prefixed IDs and rewire incoming/outgoing sequence flows.
fn flatten_subprocesses(model: BpmnProcess) -> BpmnProcess {
    let mut flat_nodes: HashMap<String, BpmnFlowNode> = HashMap::new();
    let mut flat_flows: Vec<BpmnSequenceFlow> = Vec::new();
    let mut needs_flatten = false;

    // First pass: collect non-subprocess nodes and flows as-is
    for (id, node) in &model.flow_nodes {
        if matches!(node, BpmnFlowNode::SubProcess { .. }) {
            needs_flatten = true;
        } else {
            flat_nodes.insert(id.clone(), node.clone());
        }
    }
    flat_flows.extend(model.sequence_flows.iter().cloned());

    if !needs_flatten {
        return model;
    }

    // Second pass: flatten each subprocess
    for (sp_id, node) in &model.flow_nodes {
        let BpmnFlowNode::SubProcess {
            flow_nodes,
            sequence_flows,
            incoming,
            outgoing,
            ..
        } = node
        else {
            continue;
        };

        // Find internal start and end events.
        // NOTE: internal nodes' outgoing/incoming are not populated by wire_flows
        // (it only runs on the top-level model), so we derive flow IDs from
        // the internal sequence_flows instead.
        let sp_internal_start_ids: Vec<String> = flow_nodes
            .values()
            .filter(|n| matches!(n, BpmnFlowNode::StartEvent { .. }))
            .map(|n| n.id().to_string())
            .collect();
        let internal_start_outgoing: Vec<String> = sequence_flows
            .iter()
            .filter(|f| sp_internal_start_ids.contains(&f.source_ref))
            .map(|f| f.id.clone())
            .collect();

        let sp_internal_end_ids: Vec<String> = flow_nodes
            .values()
            .filter(|n| matches!(n, BpmnFlowNode::EndEvent { .. }))
            .map(|n| n.id().to_string())
            .collect();
        let internal_end_incoming: Vec<String> = sequence_flows
            .iter()
            .filter(|f| sp_internal_end_ids.contains(&f.target_ref))
            .map(|f| f.id.clone())
            .collect();

        // Map old flow IDs to new (prefixed) flow IDs for internal flows
        let prefix = format!("{}:", sp_id);
        let mut flow_id_map: HashMap<String, String> = HashMap::new();

        // Promote internal nodes (skip start/end events)
        for (inner_id, inner_node) in flow_nodes {
            if matches!(
                inner_node,
                BpmnFlowNode::StartEvent { .. } | BpmnFlowNode::EndEvent { .. }
            ) {
                continue;
            }
            let prefixed_id = format!("{}{}", prefix, inner_id);
            let mut promoted = inner_node.clone();
            // Update the node's ID by rebuilding with prefixed id
            promoted = match promoted {
                BpmnFlowNode::ServiceTask {
                    name,
                    task_type,
                    retries,
                    timeout_secs,
                    ..
                } => BpmnFlowNode::ServiceTask {
                    id: prefixed_id.clone(),
                    name,
                    task_type,
                    retries,
                    timeout_secs,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::UserTask {
                    name,
                    form_key,
                    form_fields,
                    ..
                } => BpmnFlowNode::UserTask {
                    id: prefixed_id.clone(),
                    name,
                    form_key,
                    form_fields,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::ExclusiveGateway { name, .. } => BpmnFlowNode::ExclusiveGateway {
                    id: prefixed_id.clone(),
                    name,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::ParallelGateway { name, .. } => BpmnFlowNode::ParallelGateway {
                    id: prefixed_id.clone(),
                    name,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::SubProcess {
                    name,
                    flow_nodes: sp_inner_nodes,
                    sequence_flows: sp_inner_flows,
                    ..
                } => BpmnFlowNode::SubProcess {
                    id: prefixed_id.clone(),
                    name,
                    flow_nodes: sp_inner_nodes,
                    sequence_flows: sp_inner_flows,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::TimerIntermediateCatchEvent {
                    name, timer_type, ..
                } => BpmnFlowNode::TimerIntermediateCatchEvent {
                    id: prefixed_id.clone(),
                    name,
                    timer_type,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::BoundaryEvent {
                    name,
                    attached_to_ref,
                    event_type,
                    is_interrupting,
                    ..
                } => BpmnFlowNode::BoundaryEvent {
                    id: prefixed_id.clone(),
                    name,
                    attached_to_ref,
                    event_type,
                    is_interrupting,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::CallActivity {
                    name,
                    called_element,
                    ..
                } => BpmnFlowNode::CallActivity {
                    id: prefixed_id.clone(),
                    name,
                    called_element,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::MessageIntermediateCatchEvent {
                    name, message_name, ..
                } => BpmnFlowNode::MessageIntermediateCatchEvent {
                    id: prefixed_id.clone(),
                    name,
                    message_name,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::MessageIntermediateThrowEvent {
                    name, message_name, ..
                } => BpmnFlowNode::MessageIntermediateThrowEvent {
                    id: prefixed_id.clone(),
                    name,
                    message_name,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::SignalIntermediateThrowEvent {
                    name, signal_name, ..
                } => BpmnFlowNode::SignalIntermediateThrowEvent {
                    id: prefixed_id.clone(),
                    name,
                    signal_name,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::SignalIntermediateCatchEvent {
                    name, signal_name, ..
                } => BpmnFlowNode::SignalIntermediateCatchEvent {
                    id: prefixed_id.clone(),
                    name,
                    signal_name,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::TerminateEndEvent { name, .. } => BpmnFlowNode::TerminateEndEvent {
                    id: prefixed_id.clone(),
                    name,
                    incoming: vec![],
                    outgoing: vec![],
                },
                BpmnFlowNode::StartEvent { .. } | BpmnFlowNode::EndEvent { .. } => unreachable!(),
            };
            flat_nodes.insert(prefixed_id, promoted);
        }

        // Prefix internal flow IDs and rewrite source/target refs
        for flow in sequence_flows {
            let new_flow_id = format!("{}{}", prefix, flow.id);
            flow_id_map.insert(flow.id.clone(), new_flow_id.clone());

            let new_source = if flow_nodes.contains_key(&flow.source_ref)
                && !matches!(
                    flow_nodes.get(&flow.source_ref).unwrap(),
                    BpmnFlowNode::StartEvent { .. }
                ) {
                format!("{}{}", prefix, flow.source_ref)
            } else {
                flow.source_ref.clone()
            };

            let new_target = if flow_nodes.contains_key(&flow.target_ref)
                && !matches!(
                    flow_nodes.get(&flow.target_ref).unwrap(),
                    BpmnFlowNode::EndEvent { .. }
                ) {
                format!("{}{}", prefix, flow.target_ref)
            } else {
                flow.target_ref.clone()
            };

            flat_flows.push(BpmnSequenceFlow {
                id: new_flow_id,
                source_ref: new_source,
                target_ref: new_target,
                condition_expression: flow.condition_expression.clone(),
                is_default: flow.is_default,
            });
        }

        // Rewire: incoming flows of the subprocess → targets of internal start's outgoing flows
        for incoming_flow_id in incoming {
            let internal_targets: Vec<(String, Option<String>)> = internal_start_outgoing
                .iter()
                .filter_map(|inner_flow_id| {
                    sequence_flows
                        .iter()
                        .find(|f| &f.id == inner_flow_id)
                        .map(|f| {
                            let target = if flow_nodes.contains_key(&f.target_ref)
                                && !matches!(
                                    flow_nodes.get(&f.target_ref).unwrap(),
                                    BpmnFlowNode::EndEvent { .. }
                                ) {
                                format!("{}{}", prefix, f.target_ref)
                            } else {
                                f.target_ref.clone()
                            };
                            (target, f.condition_expression.clone())
                        })
                })
                .collect();

            // Remove the old incoming flow that pointed to the subprocess
            if let Some(flow) = flat_flows.iter().find(|f| &f.id == incoming_flow_id) {
                let source = flow.source_ref.clone();
                let flow_idx = flat_flows.iter().position(|f| &f.id == incoming_flow_id);
                if let Some(idx) = flow_idx {
                    flat_flows.remove(idx);
                }

                // Create new flows from source to each internal start target
                for (target, cond) in internal_targets {
                    flat_flows.push(BpmnSequenceFlow {
                        id: format!("{}_rewired_{}", incoming_flow_id, target),
                        source_ref: source.clone(),
                        target_ref: target,
                        condition_expression: cond,
                        is_default: false,
                    });
                }
            }
        }

        // Rewire: sources of internal end's incoming flows → subprocess outgoing targets
        for outgoing_flow_id in outgoing {
            let internal_sources: Vec<String> = internal_end_incoming
                .iter()
                .filter_map(|inner_flow_id| {
                    sequence_flows
                        .iter()
                        .find(|f| &f.id == inner_flow_id)
                        .map(|f| {
                            if flow_nodes.contains_key(&f.source_ref)
                                && !matches!(
                                    flow_nodes.get(&f.source_ref).unwrap(),
                                    BpmnFlowNode::StartEvent { .. }
                                )
                            {
                                format!("{}{}", prefix, f.source_ref)
                            } else {
                                f.source_ref.clone()
                            }
                        })
                })
                .collect();

            if let Some(flow) = flat_flows.iter().find(|f| &f.id == outgoing_flow_id) {
                let target = flow.target_ref.clone();
                let flow_idx = flat_flows.iter().position(|f| &f.id == outgoing_flow_id);
                if let Some(idx) = flow_idx {
                    flat_flows.remove(idx);
                }

                for source in internal_sources {
                    flat_flows.push(BpmnSequenceFlow {
                        id: format!("{}_rewired_{}", outgoing_flow_id, source),
                        source_ref: source,
                        target_ref: target.clone(),
                        condition_expression: None,
                        is_default: false,
                    });
                }
            }
        }

        // Remove internal flows that reference removed internal start/end events
        let internal_start_ids: HashSet<String> = flow_nodes
            .values()
            .filter(|n| matches!(n, BpmnFlowNode::StartEvent { .. }))
            .map(|n| n.id().to_string())
            .collect();
        let internal_end_ids: HashSet<String> = flow_nodes
            .values()
            .filter(|n| matches!(n, BpmnFlowNode::EndEvent { .. }))
            .map(|n| n.id().to_string())
            .collect();

        flat_flows.retain(|f| {
            let source_is_internal_start = internal_start_ids.contains(&f.source_ref);
            let target_is_internal_end = internal_end_ids.contains(&f.target_ref);
            !(source_is_internal_start || target_is_internal_end)
        });
    }

    // Recursively flatten any nested subprocesses
    let intermediate = BpmnProcess {
        id: model.id.clone(),
        name: model.name.clone(),
        flow_nodes: flat_nodes,
        sequence_flows: flat_flows,
    };

    // Check if there are still subprocesses to flatten
    let has_subprocesses = intermediate
        .flow_nodes
        .values()
        .any(|n| matches!(n, BpmnFlowNode::SubProcess { .. }));

    if has_subprocesses {
        flatten_subprocesses(intermediate)
    } else {
        intermediate
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
        if matches!(
            node,
            BpmnFlowNode::EndEvent { .. } | BpmnFlowNode::TerminateEndEvent { .. }
        ) {
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
            BpmnFlowNode::UserTask {
                form_key,
                form_fields,
                ..
            } => NodeType::UserTask {
                form_key: form_key.clone(),
                form_fields: form_fields.clone(),
            },
            BpmnFlowNode::ExclusiveGateway { .. } => NodeType::ExclusiveGateway,
            BpmnFlowNode::ParallelGateway { .. } => {
                let inc = incoming_count.get(id).copied().unwrap_or(0);
                if inc != 1 {
                    NodeType::ParallelJoin { expected: inc }
                } else {
                    NodeType::ParallelFork
                }
            }
            BpmnFlowNode::SubProcess { .. } => {
                // Should have been flattened; treat as error sentinel
                continue;
            }
            BpmnFlowNode::TimerIntermediateCatchEvent { timer_type, .. } => {
                let timer_definition = match timer_type {
                    TimerType::TimeDuration(d) => d.clone(),
                    TimerType::TimeDate(d) => d.clone(),
                    TimerType::TimeCycle(c) => c.clone(),
                };
                NodeType::TimerIntermediateCatch { timer_definition }
            }
            BpmnFlowNode::BoundaryEvent {
                event_type,
                is_interrupting,
                ..
            } => match event_type {
                BoundaryEventType::Timer(timer_type) => {
                    let duration = match timer_type {
                        TimerType::TimeDuration(d) => d.clone(),
                        TimerType::TimeDate(d) => d.clone(),
                        TimerType::TimeCycle(c) => c.clone(),
                    };
                    NodeType::BoundaryTimer {
                        duration,
                        is_interrupting: *is_interrupting,
                    }
                }
                BoundaryEventType::Error { error_code } => NodeType::BoundaryError {
                    error_code: error_code.clone(),
                    is_interrupting: *is_interrupting,
                },
            },
            BpmnFlowNode::CallActivity { called_element, .. } => NodeType::CallActivity {
                called_process_key: called_element.clone(),
            },
            BpmnFlowNode::MessageIntermediateCatchEvent { message_name, .. } => {
                NodeType::MessageIntermediateCatch {
                    message_name: message_name.clone(),
                }
            }
            BpmnFlowNode::MessageIntermediateThrowEvent { message_name, .. } => {
                NodeType::MessageIntermediateThrow {
                    message_name: message_name.clone(),
                }
            }
            BpmnFlowNode::SignalIntermediateThrowEvent { signal_name, .. } => {
                NodeType::SignalIntermediateThrow {
                    signal_name: signal_name.clone(),
                }
            }
            BpmnFlowNode::SignalIntermediateCatchEvent { signal_name, .. } => {
                NodeType::SignalIntermediateCatch {
                    signal_name: signal_name.clone(),
                }
            }
            BpmnFlowNode::TerminateEndEvent { .. } => NodeType::TerminateEnd,
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

    // Collect boundary event host refs for string leaking
    let boundary_host_refs: Vec<String> = model
        .flow_nodes
        .values()
        .filter_map(|n| {
            if let BpmnFlowNode::BoundaryEvent {
                attached_to_ref, ..
            } = n
            {
                Some(attached_to_ref.clone())
            } else {
                None
            }
        })
        .collect();

    let mut all_strings: HashSet<String> = HashSet::new();
    all_strings.insert(process_id.to_string());
    all_strings.insert(start_id.clone());
    for (id, (_, out)) in &nodes {
        all_strings.insert(id.clone());
        for (target, _) in out {
            all_strings.insert(target.clone());
        }
    }
    for h in &boundary_host_refs {
        all_strings.insert(h.clone());
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

    // Build boundary_events map: host_node_id -> Vec<BoundaryEventDef>
    let mut boundary_events: HashMap<&'static str, Vec<BoundaryEventDef>> = HashMap::new();
    for node in model.flow_nodes.values() {
        if let BpmnFlowNode::BoundaryEvent {
            id,
            attached_to_ref,
            is_interrupting,
            outgoing,
            ..
        } = node
        {
            let target_node_id = outgoing
                .first()
                .and_then(|fid| {
                    model
                        .sequence_flows
                        .iter()
                        .find(|f| &f.id == fid)
                        .map(|f| get(&f.target_ref))
                })
                .unwrap_or(get(id));
            let def = BoundaryEventDef {
                node_id: get(id),
                is_interrupting: *is_interrupting,
                target_node_id,
            };
            boundary_events
                .entry(get(attached_to_ref))
                .or_default()
                .push(def);
        }
    }

    Ok(ProcessDefinition {
        id: get(process_id),
        start: get(&start_id),
        nodes: engine_nodes,
        boundary_events,
    })
}
