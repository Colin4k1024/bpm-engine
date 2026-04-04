//! Persistent timer store: schedule, fire, and list due timers.
//!
//! # Example
//!
//! ```ignore
//! let repo = Arc::new(MemoryRepo::new());
//!
//! // Insert a timer scheduled to fire at unix second 9999
//! let record = TimerRecord {
//!     id: "timer-1".into(),
//!     token_id: "token-1".into(),
//!     instance_id: "instance-1".into(),
//!     due_at: "9999".into(),
//!     status: "Scheduled".into(),
//!     created_at: "1000".into(),
//! };
//! repo.insert(&record).await?;
//!
//! // List due timers (9999 <= 9999)
//! let due = repo.list_due("9999", 100).await?;
//! assert_eq!(due.len(), 1);
//! assert_eq!(due[0].id, "timer-1");
//!
//! // Mark as fired
//! repo.mark_fired("timer-1").await?;
//! let timer = repo.get_by_id("timer-1").await?.unwrap();
//! assert_eq!(timer.status, "Fired");
//! ```

use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct TimerRecord {
    pub id: String,
    pub token_id: String,
    pub instance_id: String,
    pub due_at: String,
    pub status: String,
    pub created_at: String,
}

/// TimerStore persists timer records for crash-safe timer execution.
///
/// Timers are created when a BPMN timer intermediate catch event or timer boundary event
/// is activated. The engine queries [`list_due`] periodically to fire expired timers.
#[async_trait]
pub trait TimerStore: Send + Sync {
    /// Load a timer by id.
    async fn get_by_id(&self, id: &str) -> anyhow::Result<Option<TimerRecord>>;

    /// Mark a timer as fired (transitions to Fired state).
    async fn mark_fired(&self, id: &str) -> anyhow::Result<()>;

    /// Insert a new timer record.
    async fn insert(&self, record: &TimerRecord) -> anyhow::Result<()>;

    /// List timers due at or before `now_iso`, up to `limit` results.
    ///
    /// Used by the engine scheduler to find timers ready to fire.
    async fn list_due(&self, now_iso: &str, limit: u32) -> anyhow::Result<Vec<TimerRecord>>;
}
