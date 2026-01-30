//! In-memory ProcessDefinition store (design: Step 3).

use crate::model::ProcessDefinition;
use std::collections::HashMap;
use std::sync::RwLock;

use super::repo::ProcessDefinitionRepo;

/// In-memory store: register definitions by id, load by id.
pub struct ProcessDefStore {
    defs: RwLock<HashMap<String, ProcessDefinition>>,
}

impl ProcessDefStore {
    pub fn new() -> Self {
        ProcessDefStore {
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

impl ProcessDefinitionRepo for ProcessDefStore {
    fn load(&self, id: &str) -> Option<ProcessDefinition> {
        self.defs.read().unwrap().get(id).cloned()
    }
}
