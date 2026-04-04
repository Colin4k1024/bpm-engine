//! Execution history for Trace UI: append events, list by instance with optional filters.
//!
//! # Example
//!
//! ```ignore
//! let repo = Arc::new(MemoryRepo::new());
//!
//! // Append a token-arrived event
//! let payload = serde_json::json!({ "token_id": "token-1", "node_id": "task-1" });
//! let id = repo
//!     .append("instance-1", "TokenArrived", &payload, "1000")
//!     .await?;
//!
//! // List all events for this instance
//! let events = repo.list_by_instance("instance-1", None, None).await?;
//! assert_eq!(events.len(), 1);
//! assert_eq!(events[0].event_type, "TokenArrived");
//!
//! // Filter by event type
//! let arrived = repo.list_by_instance("instance-1", None, Some("TokenArrived")).await?;
//! assert_eq!(arrived.len(), 1);
//!
//! // Filter by token_id (extracted from payload)
//! let token_events = repo.list_by_instance("instance-1", Some("token-1"), None).await?;
//! assert_eq!(token_events.len(), 1);
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub id: String,
    pub instance_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub occurred_at: String,
}

/// HistoryRepo records every state transition for audit and replay (design: execution-model.md §5).
///
/// Events are append-only and form a complete trace of what happened in a process instance.
/// The Trace UI and recovery mechanism both consume this log.
#[async_trait]
pub trait HistoryRepo: Send + Sync {
    /// Append one event (called when engine applies an event).
    async fn append(
        &self,
        instance_id: &str,
        event_type: &str,
        payload: &serde_json::Value,
        occurred_at: &str,
    ) -> anyhow::Result<String>;

    /// List events for an instance, optionally filtered by token_id (from payload) or event_type.
    /// Returns events sorted by occurred_at ascending.
    async fn list_by_instance(
        &self,
        instance_id: &str,
        token_id_filter: Option<&str>,
        event_type_filter: Option<&str>,
    ) -> anyhow::Result<Vec<HistoryEvent>>;
}
