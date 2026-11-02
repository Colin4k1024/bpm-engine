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
