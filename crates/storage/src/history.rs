//! Execution history for Trace UI: append events, list by instance with optional filters.

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
