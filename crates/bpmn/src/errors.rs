//! BPMN parse and compile errors.
//! Compiler uses CompilerError + ErrorCode (03.md); compile returns Vec<CompilerError>.

use serde::Serialize;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("invalid XML: {0}")]
    InvalidXml(#[from] roxmltree::Error),

    #[error("unsupported or unknown BPMN element: {0}")]
    UnknownElement(String),

    #[error("missing required attribute: {0}")]
    MissingAttribute(String),

    #[error("no process found in definitions")]
    NoProcess,

    #[error("multiple processes not supported")]
    MultipleProcesses,
}

/// Stable compiler error codes (03.md). Do not change lightly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum ErrorCode {
    NoStartEvent,
    MultipleStartEvents,
    NoEndEvent,
    OrphanNode,
    DeadEnd,
    ExclusiveGatewayNoDefault,
    ParallelGatewayInvalidShape,
    SequenceFlowSourceNotFound,
    SequenceFlowTargetNotFound,
    UnsupportedElement,
}

/// Single compiler error with context for diagnostics (03.md).
#[derive(Debug, Clone, Serialize)]
pub struct CompilerError {
    pub code: ErrorCode,
    pub message: String,
    pub node_id: Option<String>,
    pub flow_id: Option<String>,
    pub hint: Option<String>,
}

impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}", self.code, self.message)?;
        if let Some(ref n) = self.node_id {
            write!(f, " (node: {})", n)?;
        }
        if let Some(ref fl) = self.flow_id {
            write!(f, " (flow: {})", fl)?;
        }
        if let Some(ref h) = self.hint {
            write!(f, " Hint: {}", h)?;
        }
        Ok(())
    }
}

impl std::error::Error for CompilerError {}

/// Legacy single CompileError for backward compat; used when parse fails.
#[derive(Error, Debug)]
pub enum CompileError {
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),

    #[error("compile errors: {0}")]
    Compile(#[source] CompileErrors),
}

/// Opaque wrapper so thiserror can display the list.
#[derive(Debug)]
pub struct CompileErrors(pub Vec<CompilerError>);

impl std::fmt::Display for CompileErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{}", e)?;
        }
        Ok(())
    }
}

impl std::error::Error for CompileErrors {}

impl From<Vec<CompilerError>> for CompileErrors {
    fn from(v: Vec<CompilerError>) -> Self {
        CompileErrors(v)
    }
}
