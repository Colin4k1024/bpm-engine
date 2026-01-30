use async_trait::async_trait;
use bpm_core::ProcessDefinition;
use bpm_storage::ProcessDefinitionRepo;
use std::collections::HashMap;
use std::sync::RwLock;

/// In-memory process definition store (register by id, load by id).
pub struct ProcessDefStore {
    defs: RwLock<HashMap<String, ProcessDefinition>>,
}

impl ProcessDefStore {
    pub fn new() -> Self {
        Self {
            defs: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, def: ProcessDefinition) {
        self.defs
            .write()
            .unwrap()
            .insert(def.id.to_string(), def);
    }
}

impl Default for ProcessDefStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProcessDefinitionRepo for ProcessDefStore {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessDefinition>> {
        Ok(self.defs.read().unwrap().get(id).cloned())
    }
}
