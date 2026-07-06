//! Compensation records for saga-style compensation of completed activities.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use bpm_engine_storage::{CompensationRecordRepo, CompensationRecordRow};
//!
//! # async fn example(repo: Arc<impl CompensationRecordRepo>) -> anyhow::Result<()> {
//! let record = CompensationRecordRow {
//!     id: "comp-1".into(),
//!     instance_id: "instance-1".into(),
//!     node_id: "task-1".into(),
//!     handler_ref: "undo-payment".into(),
//!     order: 1,
//!     status: "Pending".into(),
//!     created_at: "1000".into(),
//! };
//! repo.add(&record).await?;
//!
//! let records = repo.list_by_instance("instance-1").await;
//! assert_eq!(records.len(), 1);
//! assert_eq!(records[0].node_id, "task-1");
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;

/// A compensation record row from the storage layer.
#[derive(Debug, Clone)]
pub struct CompensationRecordRow {
    /// Unique record identifier.
    pub id: String,
    /// The process instance this record belongs to.
    pub instance_id: String,
    /// The node that performed the compensatable action.
    pub node_id: String,
    /// Reference to the compensation handler (e.g., function name or task type).
    pub handler_ref: String,
    /// Execution order (higher = completed later = compensated first).
    pub order: u32,
    /// Current status: "Pending", "Completed", or "Failed".
    pub status: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// CompensationRecordRepo stores compensation records for saga-style rollback.
///
/// When a BPMN compensation boundary event catches an error, the engine
/// executes compensation handlers in reverse order of completion.
#[async_trait]
pub trait CompensationRecordRepo: Send + Sync {
    /// Add a compensation record.
    async fn add(&self, record: &CompensationRecordRow) -> anyhow::Result<()>;

    /// List compensation records for an instance, sorted by `order` ascending.
    async fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow>;
}
