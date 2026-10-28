use async_trait::async_trait;
use bpm_engine_core::{InstanceState, ProcessInstance};
use bpm_engine_storage::ProcessInstanceStore;
use std::collections::HashMap;
use std::sync::RwLock;

pub struct MemoryProcessStore {
    instances: RwLock<HashMap<String, ProcessInstance>>,
    /// Start-instance idempotency keys (E2-4): `(tenant_scope, key) ->
    /// instance_id`. The tenant scope is `""` for no tenant.
    idempotency_keys: RwLock<HashMap<(String, String), String>>,
}

impl MemoryProcessStore {
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
            idempotency_keys: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryProcessStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProcessInstanceStore for MemoryProcessStore {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessInstance>> {
        Ok(self.instances.read().unwrap().get(id).cloned())
    }

    async fn save(&self, instance: &ProcessInstance) -> anyhow::Result<()> {
        self.instances
            .write()
            .unwrap()
            .insert(instance.id.clone(), instance.clone());
        Ok(())
    }

    async fn list_running(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>> {
        let ids: Vec<String> = self
            .instances
            .read()
            .unwrap()
            .iter()
            .filter(|(_, i)| {
                i.state == InstanceState::Running
                    && match (tenant_id, &i.tenant_id) {
                        (None, _) => true,
                        (Some(t), Some(ti)) => t == ti.as_str(),
                        (Some(""), None) => true,
                        (Some(_), None) => false,
                    }
            })
            .map(|(id, _)| id.clone())
            .collect();
        Ok(ids)
    }

    async fn list_all(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>> {
        let mut ids: Vec<String> = self
            .instances
            .read()
            .unwrap()
            .iter()
            .filter(|(_, i)| match (tenant_id, &i.tenant_id) {
                (None, _) => true,
                (Some(t), Some(ti)) => t == ti.as_str(),
                (Some(""), None) => true,
                (Some(_), None) => false,
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        Ok(ids)
    }

    async fn list_page(
        &self,
        tenant_id: Option<&str>,
        offset: u64,
        limit: u64,
    ) -> anyhow::Result<Vec<String>> {
        // Same tenant filter and deterministic id ordering as list_all.
        let mut ids = self.list_all(tenant_id).await?;
        let offset = (offset as usize).min(ids.len());
        ids.drain(..offset);
        ids.truncate(limit as usize);
        Ok(ids)
    }

    async fn find_idempotency_key(
        &self,
        key: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let scope = tenant_id.unwrap_or("");
        Ok(self
            .idempotency_keys
            .read()
            .unwrap()
            .get(&(scope.to_string(), key.to_string()))
            .cloned())
    }

    async fn save_idempotency_key(
        &self,
        key: &str,
        tenant_id: Option<&str>,
        instance_id: &str,
    ) -> anyhow::Result<()> {
        let scope = tenant_id.unwrap_or("");
        self.idempotency_keys.write().unwrap().insert(
            (scope.to_string(), key.to_string()),
            instance_id.to_string(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_instance(id: &str) -> ProcessInstance {
        ProcessInstance {
            id: id.to_string(),
            process_def_id: "def-1".to_string(),
            tenant_id: None,
            tokens: vec![],
            variables: HashMap::new(),
            state: InstanceState::Running,
            version: 1,
            parent_instance_id: None,
            parent_token_id: None,
        }
    }

    #[tokio::test]
    async fn list_page_windows_and_saturates() {
        let store = MemoryProcessStore::new();
        for id in ["a", "b", "c", "d", "e"] {
            store.save(&make_instance(id)).await.unwrap();
        }

        let first_two = store.list_page(None, 0, 2).await.unwrap();
        assert_eq!(first_two, vec!["a", "b"]);
        let next_two = store.list_page(None, 2, 2).await.unwrap();
        assert_eq!(next_two, vec!["c", "d"]);
        // Offset beyond the end returns an empty page, not an error.
        let beyond = store.list_page(None, 10, 2).await.unwrap();
        assert!(beyond.is_empty());
        // Limit larger than the remaining entries saturates.
        let tail = store.list_page(None, 3, 100).await.unwrap();
        assert_eq!(tail, vec!["d", "e"]);
    }

    #[tokio::test]
    async fn idempotency_keys_are_tenant_scoped_and_upserted() {
        let store = MemoryProcessStore::new();
        assert_eq!(store.find_idempotency_key("k1", None).await.unwrap(), None);

        store
            .save_idempotency_key("k1", None, "inst-1")
            .await
            .unwrap();
        assert_eq!(
            store.find_idempotency_key("k1", None).await.unwrap(),
            Some("inst-1".to_string())
        );
        // The same key under another tenant is a different mapping.
        assert_eq!(
            store.find_idempotency_key("k1", Some("t2")).await.unwrap(),
            None
        );
        store
            .save_idempotency_key("k1", Some("t2"), "inst-2")
            .await
            .unwrap();
        assert_eq!(
            store.find_idempotency_key("k1", Some("t2")).await.unwrap(),
            Some("inst-2".to_string())
        );

        // Upsert: re-saving the same key overwrites the mapping.
        store
            .save_idempotency_key("k1", None, "inst-3")
            .await
            .unwrap();
        assert_eq!(
            store.find_idempotency_key("k1", None).await.unwrap(),
            Some("inst-3".to_string())
        );
    }
}
