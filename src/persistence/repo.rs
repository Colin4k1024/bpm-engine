//! Repository traits (design: overview §5.1).

use crate::model::{ProcessDefinition, ProcessInstance, Token};

/// ProcessInstanceRepo: load/save instance.
pub trait ProcessInstanceRepo {
    fn load(&self, id: &str) -> Option<ProcessInstance>;
    fn save(&self, instance: &ProcessInstance);
    /// Whitepaper §12: list instance ids with state=Running (for recovery).
    fn list_running(&self) -> Vec<String>;
}

/// TokenRepo: load/save tokens by instance (or embedded in instance save).
/// Whitepaper §11.3–11.4: CAS and Claim.
pub trait TokenRepo {
    fn load_by_instance(&self, instance_id: &str) -> Vec<Token>;
    fn save_tokens(&self, instance_id: &str, tokens: &[Token]);

    /// Update token with CAS (version). Returns true iff one row updated.
    fn update_token_cas(&self, instance_id: &str, token: &Token) -> bool;

    /// Claim token: Ready -> Executing. Returns true iff one row updated.
    fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> bool;
}

/// ProcessDefinitionRepo: load definition by id (optional; definitions may stay in memory).
pub trait ProcessDefinitionRepo {
    fn load(&self, id: &str) -> Option<ProcessDefinition>;
}

/// UserTaskRepo: complete user task (optional for v1).
pub trait UserTaskRepo {
    fn complete(&self, _task_id: &str) {}
}

/// Whitepaper §11.6: Event Outbox for reliable delivery (write in tx, dispatch after commit).
/// docs_database_schema §5: event_type + payload + status.
#[derive(Debug, Clone)]
pub struct OutboxEvent {
    pub id: String,
    pub event_type: String,
    pub payload: String,
    pub status: String, // "Pending" | "Published"
    pub created_at: Option<String>,
}

/// Whitepaper §11.7: parallel join state (group_id unique, expected, arrived_count, joined).
pub trait ParallelJoinRepo {
    /// Ensure a row exists for group_id with expected count (e.g. from Fork). Idempotent.
    fn ensure_group(&self, group_id: &str, expected: u32) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    /// Atomically increment arrived_count; if arrived_count >= expected set joined=true. Returns true iff this call set joined.
    fn try_join(&self, group_id: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
}

/// OutboxRepo: insert Pending, list Pending, mark Published.
pub trait OutboxRepo {
    fn insert_pending(&self, event_type: &str, payload: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
    fn list_pending(&self) -> Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>>;
    fn mark_published(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// Whitepaper §11.5: run a closure with process_repo and token_repo inside a single DB transaction.
pub trait TransactionScope {
    fn with_tx<'r, F, R>(&'r self, f: F) -> std::result::Result<R, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce(Box<dyn ProcessInstanceRepo + 'r>, Box<dyn TokenRepo + 'r>) -> R;
}
