//! In-memory EventInboxStore (N1-2, ADR-010).
//!
//! Stores pending message/signal deliveries in a `RwLock<Vec<PendingDelivery>>`.
//! Insert is idempotent by `dedup_key`; status transitions (`mark_consumed`,
//! `mark_expired`) are idempotent. Mirrors the TimerStore shape.

use async_trait::async_trait;
use bpm_engine_storage::{DeliveryStatus, EventInboxStore, PendingDelivery};
use std::sync::RwLock;

pub struct MemoryEventInboxStore {
    records: RwLock<Vec<PendingDelivery>>,
}

impl MemoryEventInboxStore {
    pub fn new() -> Self {
        Self {
            records: RwLock::new(Vec::new()),
        }
    }
}

impl Default for MemoryEventInboxStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Tenant matching mirrors the postgres `IS NOT DISTINCT FROM $x OR tenant_id
/// IS NULL` semantics: a specific tenant sees both its own deliveries and
/// global (no-tenant) deliveries; `None` sees only global deliveries.
fn tenant_matches(record_tenant: &Option<String>, query_tenant: Option<&str>) -> bool {
    match query_tenant {
        Some(t) => record_tenant.as_deref() == Some(t) || record_tenant.is_none(),
        None => record_tenant.is_none(),
    }
}

/// Instance matching mirrors the postgres
/// `IS NOT DISTINCT FROM $x OR target_instance_id IS NULL` semantics.
fn instance_matches(record_instance: &Option<String>, query_instance: Option<&str>) -> bool {
    match query_instance {
        Some(i) => record_instance.as_deref() == Some(i) || record_instance.is_none(),
        None => record_instance.is_none(),
    }
}

#[async_trait]
impl EventInboxStore for MemoryEventInboxStore {
    async fn insert(&self, record: &PendingDelivery) -> anyhow::Result<()> {
        let mut records = self.records.write().unwrap();
        // Idempotent by dedup_key: if a record with the same key exists, leave
        // it untouched (no-op).
        if records.iter().any(|r| r.dedup_key == record.dedup_key) {
            return Ok(());
        }
        records.push(record.clone());
        Ok(())
    }

    async fn list_pending(
        &self,
        tenant_id: Option<&str>,
        name: &str,
        target_instance_id: Option<&str>,
    ) -> anyhow::Result<Vec<PendingDelivery>> {
        let records = self.records.read().unwrap();
        let mut filtered: Vec<PendingDelivery> = records
            .iter()
            .filter(|r| r.status == DeliveryStatus::Pending)
            .filter(|r| r.name == name)
            .filter(|r| tenant_matches(&r.tenant_id, tenant_id))
            .filter(|r| instance_matches(&r.target_instance_id, target_instance_id))
            .cloned()
            .collect();
        filtered.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(filtered)
    }

    async fn list_pending_for_instance(
        &self,
        instance_id: &str,
    ) -> anyhow::Result<Vec<PendingDelivery>> {
        let records = self.records.read().unwrap();
        let mut filtered: Vec<PendingDelivery> = records
            .iter()
            .filter(|r| r.status == DeliveryStatus::Pending)
            .filter(|r| r.target_instance_id.as_deref() == Some(instance_id))
            .cloned()
            .collect();
        filtered.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(filtered)
    }

    async fn mark_consumed(&self, id: &str) -> anyhow::Result<()> {
        let mut records = self.records.write().unwrap();
        if let Some(r) = records.iter_mut().find(|r| r.id == id) {
            r.status = DeliveryStatus::Consumed;
        }
        Ok(())
    }

    async fn mark_expired(&self, id: &str) -> anyhow::Result<()> {
        let mut records = self.records.write().unwrap();
        if let Some(r) = records.iter_mut().find(|r| r.id == id) {
            r.status = DeliveryStatus::Expired;
        }
        Ok(())
    }

    async fn list_expired(&self, now: &str, limit: u32) -> anyhow::Result<Vec<PendingDelivery>> {
        let records = self.records.read().unwrap();
        let mut filtered: Vec<PendingDelivery> = records
            .iter()
            .filter(|r| r.status == DeliveryStatus::Pending)
            .filter(|r| r.expires_at.as_deref().is_some_and(|exp| exp < now))
            .take(limit as usize)
            .cloned()
            .collect();
        filtered.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_storage::DeliveryType;
    use std::collections::HashMap;

    /// Build a minimal pending delivery for testing.
    fn delivery(
        id: &str,
        dedup_key: &str,
        name: &str,
        target_instance_id: Option<&str>,
        created_at: &str,
        expires_at: Option<&str>,
    ) -> PendingDelivery {
        PendingDelivery {
            id: id.to_string(),
            tenant_id: None,
            target_instance_id: target_instance_id.map(|s| s.to_string()),
            delivery_type: DeliveryType::Message,
            name: name.to_string(),
            correlation: HashMap::new(),
            payload: HashMap::new(),
            status: DeliveryStatus::Pending,
            dedup_key: dedup_key.to_string(),
            created_at: created_at.to_string(),
            expires_at: expires_at.map(|s| s.to_string()),
        }
    }

    #[tokio::test]
    async fn insert_and_list_pending() {
        let store = MemoryEventInboxStore::new();
        store
            .insert(&delivery("d1", "k1", "order.created", None, "1", None))
            .await
            .unwrap();
        store
            .insert(&delivery("d2", "k2", "order.created", None, "2", None))
            .await
            .unwrap();

        let pending = store
            .list_pending(None, "order.created", None)
            .await
            .unwrap();
        assert_eq!(pending.len(), 2);
        // Ordered by created_at ascending.
        assert_eq!(pending[0].id, "d1");
        assert_eq!(pending[1].id, "d2");

        // A different name sees nothing.
        let other = store
            .list_pending(None, "order.cancelled", None)
            .await
            .unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn dedup_key_idempotent() {
        let store = MemoryEventInboxStore::new();
        store
            .insert(&delivery("d1", "dup-key", "order.created", None, "1", None))
            .await
            .unwrap();
        // Same dedup_key, different id — must be a no-op.
        store
            .insert(&delivery("d2", "dup-key", "order.created", None, "2", None))
            .await
            .unwrap();

        let pending = store
            .list_pending(None, "order.created", None)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1, "dedup_key prevents duplicate insert");
        assert_eq!(pending[0].id, "d1", "original record is kept");
    }

    #[tokio::test]
    async fn mark_consumed() {
        let store = MemoryEventInboxStore::new();
        store
            .insert(&delivery("d1", "k1", "order.created", None, "1", None))
            .await
            .unwrap();
        store
            .insert(&delivery("d2", "k2", "order.created", None, "2", None))
            .await
            .unwrap();

        store.mark_consumed("d1").await.unwrap();

        let pending = store
            .list_pending(None, "order.created", None)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1, "consumed delivery is excluded");
        assert_eq!(pending[0].id, "d2");

        // mark_consumed is idempotent — calling again on the same id is fine.
        store.mark_consumed("d1").await.unwrap();
    }

    #[tokio::test]
    async fn list_pending_for_instance() {
        let store = MemoryEventInboxStore::new();
        store
            .insert(&delivery(
                "d1",
                "k1",
                "order.created",
                Some("inst-a"),
                "1",
                None,
            ))
            .await
            .unwrap();
        store
            .insert(&delivery(
                "d2",
                "k2",
                "order.created",
                Some("inst-b"),
                "2",
                None,
            ))
            .await
            .unwrap();

        let for_a = store.list_pending_for_instance("inst-a").await.unwrap();
        assert_eq!(for_a.len(), 1);
        assert_eq!(for_a[0].id, "d1");

        let for_b = store.list_pending_for_instance("inst-b").await.unwrap();
        assert_eq!(for_b.len(), 1);
        assert_eq!(for_b[0].id, "d2");

        let for_c = store.list_pending_for_instance("inst-c").await.unwrap();
        assert!(for_c.is_empty());
    }

    #[tokio::test]
    async fn expired_sweeper() {
        let store = MemoryEventInboxStore::new();
        // expires_at in the past relative to `now`.
        store
            .insert(&delivery(
                "d1",
                "k1",
                "order.created",
                None,
                "1",
                Some("100"),
            ))
            .await
            .unwrap();
        // Not expired yet.
        store
            .insert(&delivery(
                "d2",
                "k2",
                "order.created",
                None,
                "2",
                Some("9999"),
            ))
            .await
            .unwrap();
        // No expiry — never expires.
        store
            .insert(&delivery("d3", "k3", "order.created", None, "3", None))
            .await
            .unwrap();

        let expired = store.list_expired("200", 100).await.unwrap();
        assert_eq!(
            expired.len(),
            1,
            "only the past-expiry delivery is returned"
        );
        assert_eq!(expired[0].id, "d1");

        // After marking expired, it is no longer pending (so not returned by
        // the sweeper again).
        store.mark_expired("d1").await.unwrap();
        let expired2 = store.list_expired("200", 100).await.unwrap();
        assert!(expired2.is_empty(), "expired delivery is no longer pending");
    }
}
