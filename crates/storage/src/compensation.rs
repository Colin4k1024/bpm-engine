//! Compensation records for saga-style compensation of completed activities.
//!
//! # Example
//!
//! ```ignore
//! let repo = Arc::new(MemoryRepo::new());
//!
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
//! ```

use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct CompensationRecordRow {
    pub id: String,
    pub instance_id: String,
    pub node_id: String,
    pub handler_ref: String,
    pub order: u32,
    pub status: String,
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
