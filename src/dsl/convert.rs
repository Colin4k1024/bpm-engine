//! DSL → ProcessDefinition converter (plan v2.0 A.3).
//! Resolves ServiceTask by name via registry; leaks string keys to satisfy NodeId = &'static str.

use crate::model::{EdgeCondition, Node, NodeType, OutgoingEdge, ProcessDefinition};
use super::{
    DslEdgeCondition, DslNode, DslNodeType, DslOutgoingEdge, DslProcessDefinition,
    ServiceTaskRegistry, ServiceTaskRegistryError,
};
use std::collections::{HashMap, HashSet};

/// Convert DSL process definition to runtime ProcessDefinition using the given registry for ServiceTask handlers.
pub fn to_process_definition(
    dsl: &DslProcessDefinition,
    registry: &ServiceTaskRegistry,
) -> Result<ProcessDefinition, ServiceTaskRegistryError> {
    let strings = collect_all_strings(dsl);
    let mut leaked: HashMap<&'static str, &'static str> = HashMap::new();
    for s in strings {
        let static_ref: &'static str = Box::leak(s.into_boxed_str());
        leaked.insert(static_ref, static_ref);
    }
    let get = |s: &str| *leaked.get(s).expect("string was collected");

    let mut nodes = HashMap::new();
    for (key, dsl_node) in &dsl.nodes {
        let node = dsl_node_to_node(dsl_node, registry, get)?;
        nodes.insert(get(key), node);
    }

    Ok(ProcessDefinition {
        id: get(&dsl.id),
        start: get(&dsl.start),
        nodes,
    })
}

fn collect_all_strings(dsl: &DslProcessDefinition) -> HashSet<String> {
    let mut set = HashSet::new();
    set.insert(dsl.id.clone());
    set.insert(dsl.start.clone());
    for (k, n) in &dsl.nodes {
        set.insert(k.clone());
        set.insert(n.id.clone());
        for e in &n.outgoing_edges {
            set.insert(e.target.clone());
        }
    }
    set
}

fn dsl_node_to_node<F>(
    dsl_node: &DslNode,
    registry: &ServiceTaskRegistry,
    get: F,
) -> Result<Node, ServiceTaskRegistryError>
where
    F: Fn(&str) -> &'static str,
{
    let node_type = match &dsl_node.node_type {
        DslNodeType::Start => NodeType::Start,
        DslNodeType::End => NodeType::End,
        DslNodeType::ServiceTask { handler_ref } => {
            let handler = registry.resolve(handler_ref)?;
            NodeType::ServiceTask(handler)
        }
        DslNodeType::UserTask => NodeType::UserTask,
        DslNodeType::ExclusiveGateway => NodeType::ExclusiveGateway,
        DslNodeType::ParallelFork => NodeType::ParallelFork,
        DslNodeType::ParallelJoin { expected } => NodeType::ParallelJoin {
            expected: *expected as usize,
        },
    };
    let outgoing_edges: Vec<OutgoingEdge> = dsl_node
        .outgoing_edges
        .iter()
        .map(|e| dsl_edge_to_edge(e, &get))
        .collect();
    Ok(Node {
        id: get(&dsl_node.id),
        node_type,
        outgoing_edges,
    })
}

fn dsl_edge_to_edge<F>(e: &DslOutgoingEdge, get: &F) -> OutgoingEdge
where
    F: Fn(&str) -> &'static str,
{
    OutgoingEdge {
        target: get(&e.target),
        condition: e.condition.as_ref().map(|c| dsl_condition_to_condition(c)),
    }
}

fn dsl_condition_to_condition(c: &DslEdgeCondition) -> EdgeCondition {
    match c {
        DslEdgeCondition::VariableEq { key, value } => EdgeCondition::VariableEq {
            key: key.clone(),
            value: value.clone(),
        },
        DslEdgeCondition::Expression(s) => EdgeCondition::Expression(s.clone()),
        DslEdgeCondition::Default => EdgeCondition::Default,
    }
}
