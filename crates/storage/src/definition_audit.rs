//! Definition lifecycle audit trail (N1-4).
//!
//! Records actor, version, timestamp, content summary and reason for every
//! deploy/activate/deprecate/retire operation. Definitions are not
//! instance-scoped, so this is a separate store from the instance
//! `HistoryRepo`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The lifecycle operation that produced an audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionAuditOperation {
    /// Definition deployed (compiled + validated + published + activated).
    Deploy,
    /// Definition activated (set as the key's current version).
    Activate,
    /// Definition auto-deactivated (another version activated for the key).
    Deactivate,
    /// Definition deprecated (legacy name for retire).
    Deprecate,
    /// Definition retired (explicitly taken out of the activatable pool, terminal).
    Retire,
}

impl std::fmt::Display for DefinitionAuditOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deploy => write!(f, "deploy"),
            Self::Activate => write!(f, "activate"),
            Self::Deactivate => write!(f, "deactivate"),
            Self::Deprecate => write!(f, "deprecate"),
            Self::Retire => write!(f, "retire"),
        }
    }
}

/// A persistent audit record for one definition lifecycle operation (N1-4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionAuditRecord {
    /// Monotonic sequence within the definition's audit log.
    pub sequence: u64,
    /// The definition id (e.g. "order-flow:3").
    pub definition_id: String,
    /// The definition key (e.g. "order-flow").
    pub definition_key: String,
    /// The version number.
    pub version: u32,
    /// What operation was performed.
    pub operation: DefinitionAuditOperation,
    /// Who performed it (verified actor from Identity).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Content summary: SHA-256 of the BPMN XML (first 16 hex) + byte size.
    /// Absent for operations that don't change content (activate/deprecate).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Byte size of the deployed BPMN XML source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_size: Option<u64>,
    /// Operator-supplied reason note (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// ISO-8601 / epoch-seconds timestamp.
    pub timestamp: String,
}

/// Persistent audit trail for definition lifecycle operations (N1-4).
#[async_trait]
pub trait DefinitionAuditStore: Send + Sync {
    /// Append an audit record. The store assigns the sequence number.
    async fn append(&self, record: DefinitionAuditRecord) -> anyhow::Result<()>;

    /// List audit records for a definition, ordered by sequence ascending.
    async fn list_by_definition(
        &self,
        definition_id: &str,
    ) -> anyhow::Result<Vec<DefinitionAuditRecord>>;

    /// List audit records for a key (all versions), ordered by sequence.
    async fn list_by_key(&self, key: &str) -> anyhow::Result<Vec<DefinitionAuditRecord>>;
}
