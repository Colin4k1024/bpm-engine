//! In-memory DefinitionAuditStore (N1-4).

use async_trait::async_trait;
use bpm_engine_storage::{DefinitionAuditRecord, DefinitionAuditStore};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

pub struct MemoryDefinitionAuditStore {
    records: RwLock<Vec<DefinitionAuditRecord>>,
    next_seq: AtomicU64,
}

impl MemoryDefinitionAuditStore {
    pub fn new() -> Self {
        Self {
            records: RwLock::new(Vec::new()),
            next_seq: AtomicU64::new(1),
        }
    }
}

impl Default for MemoryDefinitionAuditStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DefinitionAuditStore for MemoryDefinitionAuditStore {
    async fn append(&self, mut record: DefinitionAuditRecord) -> anyhow::Result<()> {
        record.sequence = self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.records.write().unwrap().push(record);
        Ok(())
    }

    async fn list_by_definition(
        &self,
        definition_id: &str,
    ) -> anyhow::Result<Vec<DefinitionAuditRecord>> {
        Ok(self
            .records
            .read()
            .unwrap()
            .iter()
            .filter(|r| r.definition_id == definition_id)
            .cloned()
            .collect())
    }

    async fn list_by_key(&self, key: &str) -> anyhow::Result<Vec<DefinitionAuditRecord>> {
        Ok(self
            .records
            .read()
            .unwrap()
            .iter()
            .filter(|r| r.definition_key == key)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_storage::DefinitionAuditOperation;

    /// Build a minimal record; `sequence` is assigned by the store on append.
    fn record(
        definition_id: &str,
        definition_key: &str,
        version: u32,
        operation: DefinitionAuditOperation,
    ) -> DefinitionAuditRecord {
        DefinitionAuditRecord {
            sequence: 0,
            definition_id: definition_id.to_string(),
            definition_key: definition_key.to_string(),
            version,
            operation,
            actor: Some("tester".to_string()),
            content_hash: None,
            content_size: None,
            reason: None,
            timestamp: "0".to_string(),
        }
    }

    #[tokio::test]
    async fn append_and_list() {
        let store = MemoryDefinitionAuditStore::new();
        store
            .append(record(
                "order-flow:1",
                "order-flow",
                1,
                DefinitionAuditOperation::Deploy,
            ))
            .await
            .unwrap();
        store
            .append(record(
                "order-flow:1",
                "order-flow",
                1,
                DefinitionAuditOperation::Activate,
            ))
            .await
            .unwrap();

        let records = store.list_by_definition("order-flow:1").await.unwrap();
        assert_eq!(records.len(), 2);
        // Sequences are monotonic and assigned in append order.
        assert_eq!(records[0].sequence, 1);
        assert_eq!(records[1].sequence, 2);
        assert_eq!(records[0].operation, DefinitionAuditOperation::Deploy);
        assert_eq!(records[1].operation, DefinitionAuditOperation::Activate);

        // A different definition id sees nothing.
        let other = store.list_by_definition("checkout:1").await.unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn list_by_key() {
        let store = MemoryDefinitionAuditStore::new();
        // Two records for the same key but different definition ids (versions).
        store
            .append(record(
                "order-flow:1",
                "order-flow",
                1,
                DefinitionAuditOperation::Deploy,
            ))
            .await
            .unwrap();
        store
            .append(record(
                "order-flow:2",
                "order-flow",
                2,
                DefinitionAuditOperation::Deploy,
            ))
            .await
            .unwrap();
        // A record for a different key must not appear.
        store
            .append(record(
                "checkout:1",
                "checkout",
                1,
                DefinitionAuditOperation::Deploy,
            ))
            .await
            .unwrap();

        let records = store.list_by_key("order-flow").await.unwrap();
        assert_eq!(records.len(), 2, "only the order-flow key's records");
        assert!(
            records.iter().any(|r| r.definition_id == "order-flow:1"),
            "includes version 1"
        );
        assert!(
            records.iter().any(|r| r.definition_id == "order-flow:2"),
            "includes version 2"
        );
        // Ordered by sequence ascending.
        assert!(records[0].sequence < records[1].sequence);
    }
}
