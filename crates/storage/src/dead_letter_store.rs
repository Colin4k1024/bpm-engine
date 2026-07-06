//! Dead letter queue store: persist and manage failed external tasks.

use async_trait::async_trait;

/// A dead letter entry representing a failed external task that exhausted retries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeadLetterEntry {
    /// Unique dead letter entry identifier.
    pub id: String,
    /// The original external task ID.
    pub task_id: String,
    /// The token associated with the failed task.
    pub token_id: String,
    /// The process instance containing the failed task.
    pub process_instance_id: String,
    /// Task type (topic) of the failed task.
    pub task_type: String,
    /// Error message from the last failed attempt.
    pub error_message: String,
    /// Process variables as serialized JSON.
    pub variables: String,
    /// Optional tenant isolation key.
    pub tenant_id: Option<String>,
    /// ISO 8601 timestamp when the entry was created.
    pub created_at: String,
}

/// Dead letter queue store for persisting failed external tasks.
#[async_trait]
pub trait DeadLetterStore: Send + Sync {
    /// Insert a dead letter entry.
    async fn insert(&self, entry: &DeadLetterEntry) -> anyhow::Result<()>;

    /// List dead letter entries, optionally filtered by tenant.
    async fn list(
        &self,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<DeadLetterEntry>>;

    /// Get a single dead letter entry by id.
    async fn get(&self, id: &str) -> anyhow::Result<Option<DeadLetterEntry>>;

    /// Requeue a dead letter entry back as an external task.
    /// Returns the requeued task id, or None if entry not found.
    async fn requeue(&self, id: &str) -> anyhow::Result<Option<String>>;

    /// Delete a dead letter entry.
    async fn delete(&self, id: &str) -> anyhow::Result<()>;
}
