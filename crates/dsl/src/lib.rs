//! Minimal declarative DSL (v1) compiled to
//! [`bpm_engine_core::ProcessDefinition`].
//!
//! The DSL is the JSON entry point for AI agents and other generators that
//! should not hand-write BPMN 2.0 XML. It supports a restricted node set
//! (`start`, `end`, `serviceTask`, `userTask`, `exclusiveGateway`,
//! `parallelGateway`, `timerIntermediateCatch`), edges with optional
//! conditions (`variableEq` / `expression`) and a `default` flag.
//!
//! Compilation lowers the document onto the **same** [`bpm_engine_bpmn`]
//! intermediate model the XML parser produces and then reuses
//! [`bpm_engine_bpmn::compile`] verbatim, so both entry points share one
//! validation and execution semantics (ADR-006). Invalid documents return a
//! [`DslCompileReport`] collecting all errors; each [`DslError`] carries a
//! stable snake_case [`DslErrorCode`] and a JSON-pointer-style `path` such as
//! `/nodes/3/type`.
//!
//! # Example
//!
//! ```
//! let json = r#"{
//!   "schema_version": 1,
//!   "process": { "id": "order" },
//!   "nodes": [
//!     { "id": "start", "type": "start" },
//!     { "id": "work", "type": "serviceTask", "task_type": "orders" },
//!     { "id": "end", "type": "end" }
//!   ],
//!   "edges": [
//!     { "source": "start", "target": "work" },
//!     { "source": "work", "target": "end" }
//!   ]
//! }"#;
//!
//! let def = bpm_engine_dsl::compile(json).unwrap();
//! assert_eq!(def.id, "order");
//! assert_eq!(def.start, "start");
//! assert_eq!(def.nodes.len(), 3);
//! ```
//!
//! The machine-readable JSON Schema for the document format is available via
//! [`schema_json`] (committed copy: `crates/dsl/schema/bpm-dsl-v1.json`).

#![warn(missing_docs)]

mod error;
mod lower;
mod schema;
mod types;
mod validate;

pub use error::{DslCompileReport, DslError, DslErrorCode};
pub use schema::schema_json;
pub use types::{
    DslCondition, DslDocument, DslEdge, DslFormField, DslFormFieldType, DslNode, DslNodeType,
    DslProcess, DslTimer,
};

/// DSL document version supported by this compiler.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Compile a DSL JSON document into a [`bpm_engine_core::ProcessDefinition`].
///
/// Pipeline (ADR-006): JSON deserialization with field-level path tracking
/// (`deny_unknown_fields`), structural checks (duplicate ids, dangling
/// references, per-type fields — all collected), then lowering onto the
/// shared BPMN intermediate model and compilation via
/// [`bpm_engine_bpmn::compile`] so semantic validation (start/end counts,
/// gateway shapes, dead ends, ...) is identical to the BPMN XML entry.
///
/// On failure, returns a [`DslCompileReport`] with every collected error.
/// The JSON-shape layer reports at most one error (streaming deserialization
/// stops at the first problem); structural and semantic layers collect all.
pub fn compile(dsl_json: &str) -> Result<bpm_engine_core::ProcessDefinition, DslCompileReport> {
    let doc = match validate::parse_document(dsl_json) {
        Ok(doc) => doc,
        Err(e) => return Err(DslCompileReport { errors: vec![e] }),
    };

    let errors = validate::validate_structure(&doc);
    if !errors.is_empty() {
        return Err(DslCompileReport { errors });
    }

    let node_index: std::collections::HashMap<&str, usize> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    let edge_index: std::collections::HashMap<String, usize> = doc
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| (lower::resolved_edge_id(e, i), i))
        .collect();

    let model = lower::lower(&doc);
    match bpm_engine_bpmn::compile(model) {
        Ok(def) => Ok(def),
        Err(compiler_errors) => {
            let errors: Vec<DslError> = compiler_errors
                .iter()
                .map(|ce| {
                    let path = ce
                        .node_id
                        .as_ref()
                        .and_then(|id| node_index.get(id.as_str()))
                        .map(|i| format!("/nodes/{}", i))
                        .or_else(|| {
                            ce.flow_id
                                .as_ref()
                                .and_then(|id| edge_index.get(id))
                                .map(|i| format!("/edges/{}", i))
                        })
                        .unwrap_or_else(|| "/".to_string());
                    DslError {
                        code: DslErrorCode::Compile,
                        path,
                        message: ce.message.clone(),
                        hint: ce.hint.clone(),
                        source_code: Some(format!("{:?}", ce.code)),
                    }
                })
                .collect();
            Err(DslCompileReport { errors })
        }
    }
}
