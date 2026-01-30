//! BPMN 2.0 compatibility layer (plan v2.0 C.2).
//! Parses minimal BPMN-like JSON and converts to DSL, then to ProcessDefinition.

use crate::dsl::{to_process_definition, DslEdgeCondition, DslNode, DslNodeType, DslOutgoingEdge, DslProcessDefinition, ServiceTaskRegistry, ServiceTaskRegistryError};
use crate::model::ProcessDefinition;
use serde::Deserialize;
use std::collections::HashMap;

/// Minimal BPMN-like JSON: nodes with type and outgoing edges.
#[derive(Debug, Deserialize)]
pub struct BpmnProcess {
    pub id: String,
    #[serde(rename = "startNodeId")]
    pub start_node_id: String,
    pub nodes: Vec<BpmnNode>,
}

#[derive(Debug, Deserialize)]
pub struct BpmnNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub handler_ref: Option<String>,
    #[serde(default)]
    pub expected: Option<u32>,
    #[serde(default)]
    pub outgoing: Vec<BpmnFlow>,
}

#[derive(Debug, Deserialize)]
pub struct BpmnFlow {
    pub target: String,
    #[serde(default)]
    pub condition: Option<BpmnCondition>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BpmnCondition {
    VariableEq { key: String, value: String },
    Expression(String),
    Default,
}

/// Parse BPMN-like JSON and convert to ProcessDefinition using the given registry.
pub fn load_bpmn_json(
    json: &str,
    registry: &ServiceTaskRegistry,
) -> Result<ProcessDefinition, BpmnLoadError> {
    let bpmn: BpmnProcess = serde_json::from_str(json).map_err(BpmnLoadError::Json)?;
    let dsl = bpmn_to_dsl(&bpmn)?;
    to_process_definition(&dsl, registry).map_err(BpmnLoadError::Registry)
}

fn bpmn_to_dsl(bpmn: &BpmnProcess) -> Result<DslProcessDefinition, BpmnLoadError> {
    let mut nodes = HashMap::new();
    for n in &bpmn.nodes {
        let node_type = match n.node_type.as_str() {
            "StartEvent" | "Start" => DslNodeType::Start,
            "EndEvent" | "End" => DslNodeType::End,
            "UserTask" => DslNodeType::UserTask,
            "ServiceTask" => DslNodeType::ServiceTask {
                handler_ref: n.handler_ref.clone().unwrap_or_else(|| "default".to_string()),
            },
            "ExclusiveGateway" => DslNodeType::ExclusiveGateway,
            "ParallelFork" => DslNodeType::ParallelFork,
            "ParallelJoin" => DslNodeType::ParallelJoin {
                expected: n.expected.unwrap_or(2),
            },
            "ParallelGateway" => {
                if n.expected.is_some() {
                    DslNodeType::ParallelJoin {
                        expected: n.expected.unwrap(),
                    }
                } else {
                    DslNodeType::ParallelFork
                }
            }
            _ => return Err(BpmnLoadError::UnsupportedNodeType(n.node_type.clone())),
        };
        let outgoing_edges: Vec<DslOutgoingEdge> = n
            .outgoing
            .iter()
            .map(|f| DslOutgoingEdge {
                target: f.target.clone(),
                condition: f.condition.as_ref().map(|c| match c {
                    BpmnCondition::VariableEq { key, value } => DslEdgeCondition::VariableEq {
                        key: key.clone(),
                        value: value.clone(),
                    },
                    BpmnCondition::Expression(s) => DslEdgeCondition::Expression(s.clone()),
                    BpmnCondition::Default => DslEdgeCondition::Default,
                }),
            })
            .collect();
        nodes.insert(
            n.id.clone(),
            DslNode {
                id: n.id.clone(),
                node_type,
                outgoing_edges,
            },
        );
    }
    Ok(DslProcessDefinition {
        id: bpmn.id.clone(),
        start: bpmn.start_node_id.clone(),
        nodes,
    })
}

#[derive(Debug)]
pub enum BpmnLoadError {
    Json(serde_json::Error),
    Registry(ServiceTaskRegistryError),
    UnsupportedNodeType(String),
}

impl std::fmt::Display for BpmnLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BpmnLoadError::Json(e) => write!(f, "JSON: {}", e),
            BpmnLoadError::Registry(e) => write!(f, "{}", e),
            BpmnLoadError::UnsupportedNodeType(t) => write!(f, "unsupported node type: {}", t),
        }
    }
}

impl std::error::Error for BpmnLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BpmnLoadError::Json(e) => Some(e),
            BpmnLoadError::Registry(e) => Some(e),
            BpmnLoadError::UnsupportedNodeType(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpmn_json_to_dsl() {
        let json = r#"
        {
            "id": "minimal",
            "startNodeId": "start",
            "nodes": [
                { "id": "start", "type": "StartEvent", "outgoing": [{ "target": "end" }] },
                { "id": "end", "type": "EndEvent", "outgoing": [] }
            ]
        }
        "#;
        let bpmn: BpmnProcess = serde_json::from_str(json).unwrap();
        let dsl = bpmn_to_dsl(&bpmn).unwrap();
        assert_eq!(dsl.id, "minimal");
        assert_eq!(dsl.start, "start");
        assert_eq!(dsl.nodes.len(), 2);
    }
}
