//! Repository traits (design: overview §5.1).

use crate::model::{ProcessDefinition, ProcessInstance, Token};

/// ProcessInstanceRepo: load/save instance.
pub trait ProcessInstanceRepo {
    fn load(&self, id: &str) -> Option<ProcessInstance>;
    fn save(&self, instance: &ProcessInstance);
    /// Whitepaper §12: list instance ids with state=Running (for recovery).
    /// v3: tenant_id = None means all tenants (recovery); Some(t) filters by tenant.
    fn list_running(&self, tenant_id: Option<&str>) -> Vec<String>;
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
/// docs_database_schema §5: event_type + payload + status. v3: tenant_id for multi-tenant.
#[derive(Debug, Clone)]
pub struct OutboxEvent {
    pub id: String,
    pub event_type: String,
    pub payload: String,
    pub status: String, // "Pending" | "Dispatched" | "Published"
    pub tenant_id: Option<String>,
    pub created_at: Option<String>,
}

/// Whitepaper §11.7: parallel join state (group_id unique, expected, arrived_count, joined).
pub trait ParallelJoinRepo {
    /// Ensure a row exists for group_id with expected count (e.g. from Fork). Idempotent.
    fn ensure_group(&self, group_id: &str, expected: u32) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    /// Atomically increment arrived_count; if arrived_count >= expected set joined=true. Returns true iff this call set joined.
    fn try_join(&self, group_id: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
}

/// OutboxRepo: insert Pending, list Pending, mark Published. v3: tenant-scoped; claim_pending for distributed.
pub trait OutboxRepo {
    fn insert_pending(&self, tenant_id: Option<&str>, event_type: &str, payload: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
    /// tenant_id = None means all tenants (legacy).
    fn list_pending(&self, tenant_id: Option<&str>) -> Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>>;
    fn mark_published(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    /// v3: claim up to `limit` Pending events for this worker; returns claimed events (status -> Dispatched).
    fn claim_pending(&self, worker_id: &str, tenant_id: Option<&str>, limit: u32) -> Result<Vec<OutboxEvent>, Box<dyn std::error::Error + Send + Sync>>;
    /// v3: release claimed event back to Pending (e.g. worker died before mark_published).
    fn release_claimed(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// Timer record (docs_database_schema §6: id, token_id, due_at, status, created_at).
/// Plan: instance_id included for loading instance on TimerFired.
#[derive(Debug, Clone)]
pub struct TimerRecord {
    pub id: String,
    pub token_id: String,
    pub instance_id: String,
    pub due_at: String,
    pub status: String, // "Scheduled" | "Fired" | "Cancelled"
    pub created_at: String,
}

/// TimerRepo: get by id, mark fired, insert (design: timer.md, docs_database_schema §6).
/// v3: list_due for timer_poller (status=Scheduled, due_at <= now).
pub trait TimerRepo {
    fn get_by_id(&self, id: &str) -> Option<TimerRecord>;
    fn mark_fired(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn insert(&self, record: &TimerRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    /// v3: list up to limit timers that are Scheduled and due_at <= now_iso (for timer poller).
    fn list_due(&self, now_iso: &str, limit: u32) -> Result<Vec<TimerRecord>, Box<dyn std::error::Error + Send + Sync>>;
}

/// Compensation record row (docs_database_schema §7: id, instance_id, node_id, handler_ref, order, status, created_at).
#[derive(Debug, Clone)]
pub struct CompensationRecordRow {
    pub id: String,
    pub instance_id: String,
    pub node_id: String,
    pub handler_ref: String,
    /// Order for reverse compensation (persisted as sort_order in DB).
    pub order: u32,
    pub status: String, // "Pending" | "Completed" | "Failed"
    pub created_at: String,
}

/// CompensationRecordRepo: add record, list by instance (ordered by order for reverse compensation).
pub trait CompensationRecordRepo {
    fn add(&self, record: &CompensationRecordRow) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow>;
}

/// Whitepaper §11.5: run a closure with process_repo and token_repo inside a single DB transaction.
pub trait TransactionScope {
    fn with_tx<'r, F, R>(&'r self, f: F) -> std::result::Result<R, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce(Box<dyn ProcessInstanceRepo + 'r>, Box<dyn TokenRepo + 'r>) -> R;
}

/// v3: Leader lease for outbox_publisher / timer_poller. DB-backed election.
pub trait LeaderLeaseRepo {
    /// Try to acquire or renew lease for role. Returns true iff this worker holds the lease.
    /// TTL is seconds from now for expires_at.
    fn try_acquire(&self, role: &str, worker_id: &str, ttl_secs: u64) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
    /// Renew lease (update expires_at). Returns true iff this worker held the lease and renewal succeeded.
    fn renew(&self, role: &str, worker_id: &str, ttl_secs: u64) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
}
