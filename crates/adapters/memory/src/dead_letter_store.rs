//! In-memory DeadLetterStore implementation.

use async_trait::async_trait;
use bpm_engine_storage::{DeadLetterEntry, DeadLetterStore};
use std::collections::HashMap;
use std::sync::RwLock;

#[cfg(test)]
fn utc_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string()
}

/// In-memory dead letter queue store.
pub struct DeadLetterRepo {
    entries: RwLock<HashMap<String, DeadLetterEntry>>,
}

impl DeadLetterRepo {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for DeadLetterRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DeadLetterStore for DeadLetterRepo {
    async fn insert(&self, entry: &DeadLetterEntry) -> anyhow::Result<()> {
        self.entries
            .write()
            .unwrap()
            .insert(entry.id.clone(), entry.clone());
        Ok(())
    }

    async fn list(
        &self,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<DeadLetterEntry>> {
        let guard = self.entries.read().unwrap();
        let limit = limit.min(1000) as usize;
        let out: Vec<DeadLetterEntry> = guard
            .values()
            .filter(|e| match (tenant_id, &e.tenant_id) {
                (None, _) => true,
                (Some(t), Some(ti)) => t == ti.as_str(),
                (Some(""), None) => true,
                (Some(_), None) => false,
            })
            .take(limit)
            .cloned()
            .collect();
        Ok(out)
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<DeadLetterEntry>> {
        Ok(self.entries.read().unwrap().get(id).cloned())
    }

    async fn requeue(&self, id: &str) -> anyhow::Result<Option<String>> {
        // Requeue removes the entry and returns its task_id for re-creation.
        let entry = self.entries.write().unwrap().remove(id);
        Ok(entry.map(|e| e.task_id))
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.entries.write().unwrap().remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: &str, task_type: &str) -> DeadLetterEntry {
        DeadLetterEntry {
            id: id.to_string(),
            task_id: format!("task-{id}"),
            token_id: "tok-1".to_string(),
            process_instance_id: "inst-1".to_string(),
            task_type: task_type.to_string(),
            error_message: "timeout".to_string(),
            variables: "{}".to_string(),
            tenant_id: None,
            created_at: utc_now(),
        }
    }

    #[tokio::test]
    async fn insert_list_get_delete() {
        let repo = DeadLetterRepo::new();
        repo.insert(&make_entry("dl-1", "payment")).await.unwrap();
        repo.insert(&make_entry("dl-2", "notify")).await.unwrap();

        let all = repo.list(None, 100).await.unwrap();
        assert_eq!(all.len(), 2);

        let one = repo.get("dl-1").await.unwrap().unwrap();
        assert_eq!(one.task_type, "payment");

        repo.delete("dl-1").await.unwrap();
        assert!(repo.get("dl-1").await.unwrap().is_none());
        assert_eq!(repo.list(None, 100).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn requeue_removes_and_returns_task_id() {
        let repo = DeadLetterRepo::new();
        repo.insert(&make_entry("dl-3", "email")).await.unwrap();

        let task_id = repo.requeue("dl-3").await.unwrap();
        assert_eq!(task_id.as_deref(), Some("task-dl-3"));
        assert!(repo.get("dl-3").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn tenant_filter() {
        let repo = DeadLetterRepo::new();
        let mut e1 = make_entry("dl-4", "a");
        e1.tenant_id = Some("t1".to_string());
        let mut e2 = make_entry("dl-5", "b");
        e2.tenant_id = Some("t2".to_string());
        repo.insert(&e1).await.unwrap();
        repo.insert(&e2).await.unwrap();

        let t1 = repo.list(Some("t1"), 100).await.unwrap();
        assert_eq!(t1.len(), 1);
        assert_eq!(t1[0].id, "dl-4");
    }
}
