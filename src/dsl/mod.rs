//! DSL for process definitions (plan v2.0 phase A).
//! Serializable JSON/YAML model; converted to model::ProcessDefinition at runtime.

mod convert;
mod load;
mod registry;

pub use convert::to_process_definition;
pub use load::{load_and_register_json, load_and_register_json_file, load_from_json, LoadError};
pub use registry::{ServiceTaskRegistry, ServiceTaskRegistryError};

use serde::Deserialize;
use std::collections::HashMap;

/// Process definition in DSL form (all ids are String for deserialization).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DslProcessDefinition {
    pub id: String,
    pub start: String,
    pub nodes: HashMap<String, DslNode>,
}

/// Single node in DSL; node_type determines which fields are used.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DslNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: DslNodeType,
    #[serde(default)]
    pub outgoing_edges: Vec<DslOutgoingEdge>,
}

/// Node type as string enum; ServiceTask and ParallelJoin have extra fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum DslNodeType {
    Start,
    End,
    ServiceTask {
        #[serde(rename = "handler_ref")]
        handler_ref: String,
    },
    UserTask,
    ExclusiveGateway,
    ParallelFork,
    ParallelJoin {
        expected: u32,
    },
}

/// Outgoing edge in DSL.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DslOutgoingEdge {
    pub target: String,
    #[serde(default)]
    pub condition: Option<DslEdgeCondition>,
}

/// Condition on an edge (ExclusiveGateway); JSON-friendly.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DslEdgeCondition {
    VariableEq { key: String, value: String },
    Expression(String),
    Default,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsl_deserialize_minimal_json() {
        let json = r#"
        {
            "id": "minimal",
            "start": "start",
            "nodes": {
                "start": {
                    "id": "start",
                    "type": { "kind": "Start" },
                    "outgoing_edges": [{ "target": "end" }]
                },
                "end": {
                    "id": "end",
                    "type": { "kind": "End" },
                    "outgoing_edges": []
                }
            }
        }
        "#;
        let dsl: DslProcessDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(dsl.id, "minimal");
        assert_eq!(dsl.start, "start");
        assert_eq!(dsl.nodes.len(), 2);
    }

    #[test]
    fn dsl_to_process_definition_minimal() {
        use super::{to_process_definition, ServiceTaskRegistry};
        let json = r#"
        {
            "id": "minimal",
            "start": "start",
            "nodes": {
                "start": {
                    "id": "start",
                    "type": { "kind": "Start" },
                    "outgoing_edges": [{ "target": "end" }]
                },
                "end": {
                    "id": "end",
                    "type": { "kind": "End" },
                    "outgoing_edges": []
                }
            }
        }
        "#;
        let dsl: DslProcessDefinition = serde_json::from_str(json).unwrap();
        let registry = ServiceTaskRegistry::new();
        let def = to_process_definition(&dsl, &registry).unwrap();
        assert_eq!(def.id, "minimal");
        assert_eq!(def.start, "start");
        assert_eq!(def.nodes.len(), 2);
    }

    #[test]
    fn dsl_deserialize_service_task_and_parallel_join() {
        let json = r#"
        {
            "id": "p1",
            "start": "start",
            "nodes": {
                "start": {
                    "id": "start",
                    "type": { "kind": "Start" },
                    "outgoing_edges": [{ "target": "task1" }]
                },
                "task1": {
                    "id": "task1",
                    "type": { "kind": "ServiceTask", "handler_ref": "validate" },
                    "outgoing_edges": [{ "target": "end" }]
                },
                "end": {
                    "id": "end",
                    "type": { "kind": "End" },
                    "outgoing_edges": []
                }
            }
        }
        "#;
        let dsl: DslProcessDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(dsl.nodes.get("task1").unwrap().id, "task1");
        match &dsl.nodes.get("task1").unwrap().node_type {
            DslNodeType::ServiceTask { handler_ref } => assert_eq!(handler_ref, "validate"),
            _ => panic!("expected ServiceTask"),
        }
    }
}
