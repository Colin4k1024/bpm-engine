//! Outbox store: reliable event publishing (outbox pattern for at-least-once delivery).
//!
//! # Example
//!
//! ```ignore
//! let repo = Arc::new(MemoryRepo::new());
//!
//! // Insert a pending event
//! let id = repo
//!     .insert_pending(None, "ExternalTaskCompleted", r#"{"task_id":"t1"}"#)
//!     .await?;
//! assert!(!id.is_empty());
//!
//! // List pending events
//! let pending = repo.list_pending(None).await?;
//! assert_eq!(pending.len(), 1);
//!
//! // Mark as published after broker delivery
//! repo.mark_published(&id).await?;
//! let updated = repo.list_pending(None).await?;
//! assert!(updated.is_empty());
//! ```

use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct OutboxEvent {
    pub id: String,
    pub event_type: String,
    pub payload: String,
    pub status: String,
    pub tenant_id: Option<String>,
    pub created_at: Option<String>,
}

/// OutboxRepo implements the transactional outbox pattern for reliable event delivery.
///
/// The BPM engine writes events to the outbox in the same transaction as state changes.
/// A separate relay process reads pending events and publishes them to the message broker.
#[async_trait]
pub trait OutboxRepo: Send + Sync {
    /// Insert a new pending event.
    async fn insert_pending(
        &self,
        tenant_id: Option<&str>,
        event_type: &str,
        payload: &str,
    ) -> anyhow::Result<String>;

    /// List events with Pending status, optionally filtered by tenant.
    async fn list_pending(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<OutboxEvent>>;

    /// Mark an event as Published (after successful broker delivery).
    async fn mark_published(&self, id: &str) -> anyhow::Result<()>;

    /// Claim pending events for relay (transitions to Dispatched state).
    async fn claim_pending(
        &self,
        worker_id: &str,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<OutboxEvent>>;

    /// Release a claimed event back to Pending (on relay failure).
    async fn release_claimed(&self, id: &str) -> anyhow::Result<()>;
}
