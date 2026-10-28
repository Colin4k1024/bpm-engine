//! Stable error codes and field-level diagnostics for the BPM DSL compiler.
//!
//! Every diagnostic carries a machine-readable [`DslErrorCode`] (snake_case,
//! stable — do not rename) and a JSON-pointer-style `path` (e.g. `/nodes/3/type`)
//! pointing at the offending location in the source DSL document.
//! See ADR-006 for the three-layer error contract.

use serde::Serialize;

/// Stable machine-readable error codes for DSL compilation.
///
/// The string form is the snake_case name of the variant (e.g. `unknown_field`).
/// Codes are part of the public contract: do not rename or reuse them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DslErrorCode {
    /// The input is not valid JSON (includes truncated input).
    JsonParse,
    /// `schema_version` is not the version supported by this compiler.
    SchemaVersionUnsupported,
    /// A field that is not part of the DSL schema (denied via `deny_unknown_fields`).
    UnknownField,
    /// A required field is absent.
    MissingField,
    /// A field has the wrong type, an unknown enum/variant value, an empty
    /// string where a non-empty one is required, or a value not valid for the
    /// node/edge type carrying it.
    InvalidType,
    /// Two nodes share the same `id`.
    DuplicateNodeId,
    /// Two edges resolve to the same edge id.
    DuplicateEdgeId,
    /// An edge `source`/`target` references a node id that does not exist.
    UnknownNodeRef,
    /// The `nodes` array is empty.
    EmptyNodeList,
    /// A semantic error collected by the shared BPMN compiler, passed through
    /// with the upstream code in [`DslError::source_code`].
    Compile,
}

impl std::fmt::Display for DslErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DslErrorCode::JsonParse => "json_parse",
            DslErrorCode::SchemaVersionUnsupported => "schema_version_unsupported",
            DslErrorCode::UnknownField => "unknown_field",
            DslErrorCode::MissingField => "missing_field",
            DslErrorCode::InvalidType => "invalid_type",
            DslErrorCode::DuplicateNodeId => "duplicate_node_id",
            DslErrorCode::DuplicateEdgeId => "duplicate_edge_id",
            DslErrorCode::UnknownNodeRef => "unknown_node_ref",
            DslErrorCode::EmptyNodeList => "empty_node_list",
            DslErrorCode::Compile => "compile",
        };
        f.write_str(s)
    }
}

/// A single field-level diagnostic produced while compiling a DSL document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DslError {
    /// Stable machine-readable error code.
    pub code: DslErrorCode,
    /// JSON-pointer-style path into the source DSL document (e.g. `/nodes/3/type`).
    pub path: String,
    /// Human-readable description of the problem.
    pub message: String,
    /// Optional actionable hint.
    pub hint: Option<String>,
    /// Upstream `bpm_engine_bpmn::ErrorCode` name (e.g. `NoEndEvent`) when
    /// `code` is [`DslErrorCode::Compile`]; `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_code: Option<String>,
}

impl DslError {
    /// Create a diagnostic with a code, path, and message (no hint).
    pub(crate) fn new(
        code: DslErrorCode,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        DslError {
            code,
            path: path.into(),
            message: message.into(),
            hint: None,
            source_code: None,
        }
    }

    /// Attach an actionable hint.
    pub(crate) fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for DslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.code, self.path, self.message)?;
        if let Some(ref hint) = self.hint {
            write!(f, " Hint: {}", hint)?;
        }
        Ok(())
    }
}

impl std::error::Error for DslError {}

/// Report collecting **all** errors found while compiling a DSL document
/// (structural errors and passed-through semantic errors; the JSON-shape layer
/// reports at most one error because streaming deserialization stops at the
/// first syntax/data error).
///
/// This is the `Err` payload of [`crate::compile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DslCompileReport {
    /// Every diagnostic, in document order.
    pub errors: Vec<DslError>,
}

impl DslCompileReport {
    /// Returns `true` when no errors were collected.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Number of collected errors.
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Returns the first error, if any.
    pub fn first(&self) -> Option<&DslError> {
        self.errors.first()
    }
}

impl std::fmt::Display for DslCompileReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, e) in self.errors.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{}", e)?;
        }
        Ok(())
    }
}

impl std::error::Error for DslCompileReport {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.errors.first().map(|e| e as &dyn std::error::Error)
    }
}

impl From<Vec<DslError>> for DslCompileReport {
    fn from(errors: Vec<DslError>) -> Self {
        DslCompileReport { errors }
    }
}
