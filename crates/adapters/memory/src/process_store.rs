use async_trait::async_trait;
use bpm_engine_core::{InstanceState, ProcessInstance};
use bpm_engine_storage::ProcessInstanceStore;
use std::collections::HashMap;
use std::sync::RwLock;

pub struct MemoryProcessStore {
    instances: RwLock<HashMap<String, ProcessInstance>>,
}

impl MemoryProcessStore {
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
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
}
