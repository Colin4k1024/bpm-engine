//! Persistent event inbox for message/signal delivery (N1-2, ADR-010).
//!
//! Stores pending message/signal deliveries that arrive before a catch
//! token is ready, or while a catch token is paused (Suspended). Mirrors
//! the TimerStore shape: insert + list_pending + mark_consumed.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Whether the delivery is a targeted message or a broadcast signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryType {
    /// Targeted delivery to one instance.
    Message,
    /// Broadcast to all matching instances.
    Signal,
}

/// Status of a pending delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// Waiting to be consumed by a catch token.
    Pending,
    /// Consumed by a catch token (terminal).
    Consumed,
    /// Expired (TTL passed) or instance cancelled (terminal).
    Expired,
}

/// A pending message/signal delivery in the inbox (ADR-010 §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDelivery {
    /// Unique delivery id (caller-supplied or generated).
    pub id: String,
    /// Tenant scope (None = global).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Target instance (None = signal broadcast, not yet matched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_instance_id: Option<String>,
    /// Message or signal.
    pub delivery_type: DeliveryType,
    /// Message name or signal name.
    pub name: String,
    /// Correlation predicates (message only): variable key → expected value.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub correlation: HashMap<String, String>,
    /// Variables to inject on consumption.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub payload: HashMap<String, String>,
    /// Current status.
    pub status: DeliveryStatus,
    /// Idempotency key (same delivery → same key → no duplicate).
    pub dedup_key: String,
    /// ISO-8601 / epoch-seconds creation timestamp.
    pub created_at: String,
    /// TTL: when the delivery expires (None = never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Persistent event inbox store (ADR-010 §2, TimerStore-shaped).
#[async_trait]
pub trait EventInboxStore: Send + Sync {
    /// Insert a pending delivery. Idempotent by `dedup_key` (same key →
    /// no duplicate, existing record untouched).
    async fn insert(&self, record: &PendingDelivery) -> anyhow::Result<()>;

    /// List pending deliveries matching a name and tenant, optionally for a
    /// specific instance. Ordered by created_at ascending.
    async fn list_pending(
        &self,
        tenant_id: Option<&str>,
        name: &str,
        target_instance_id: Option<&str>,
    ) -> anyhow::Result<Vec<PendingDelivery>>;

    /// List all pending deliveries for an instance (for resume sweep).
    async fn list_pending_for_instance(
        &self,
        instance_id: &str,
    ) -> anyhow::Result<Vec<PendingDelivery>>;

    /// Mark a delivery as consumed (idempotent).
    async fn mark_consumed(&self, id: &str) -> anyhow::Result<()>;

    /// Mark a delivery as expired (idempotent).
    async fn mark_expired(&self, id: &str) -> anyhow::Result<()>;

    /// List expired deliveries (for TTL sweeper). `now` is the current
    /// ISO-8601/epoch string; deliveries with expires_at < now are returned.
    async fn list_expired(&self, now: &str, limit: u32) -> anyhow::Result<Vec<PendingDelivery>>;
}
