//! Token: execution right + position (design: core.md, token.md).
//! TokenStatus, TokenMode, id, version (CAS), parallel_group_id, updated_at.

use crate::model::Token as LegacyToken;

/// Token identity (design: token has id).
pub type TokenId = String;

/// Parallel group for Fork/Join (design: core.md §5).
pub type ParallelGroupId = String;

/// Token lifecycle states (design: core.md §2). Re-export from model for domain use.
pub use crate::model::TokenStatus;

/// Token execution mode (design: saga, whitepaper §4). Re-export from model.
pub use crate::model::TokenMode;

/// Token = execution right + position; design fields for CAS and parallel.
#[derive(Debug, Clone)]
pub struct Token {
    pub id: TokenId,
    pub node_id: String,
    pub status: TokenStatus,
    pub mode: TokenMode,
    /// Optimistic lock (design: token.md).
    pub version: u32,
    /// Retry/attempt counter (docs_database_schema §3).
    pub attempt: u32,
    /// For Parallel Fork/Join (design: core.md §5).
    pub parallel_group_id: Option<ParallelGroupId>,
    /// Timestamp of last update (recovery, whitepaper §12).
    pub updated_at: Option<String>,
}

impl Token {
    /// Valid state transitions and update (design: core.md §9).
    pub fn transition(&mut self, to: TokenStatus) {
        self.status = to;
    }

    pub fn from_legacy(legacy: &LegacyToken) -> Self {
        Token {
            id: legacy.id.clone(),
            node_id: legacy.node_id.clone(),
            status: legacy.status,
            mode: legacy.mode,
            version: legacy.version,
            attempt: legacy.attempt,
            parallel_group_id: legacy.parallel_group_id.clone(),
            updated_at: legacy.updated_at.clone(),
        }
    }

    pub fn to_legacy(&self) -> LegacyToken {
        LegacyToken {
            id: self.id.clone(),
            node_id: self.node_id.clone(),
            status: self.status,
            mode: self.mode,
            version: self.version,
            attempt: self.attempt,
            parallel_group_id: self.parallel_group_id.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}
