use async_trait::async_trait;
use bpm_engine_core::ProcessDefinition;
use bpm_engine_storage::{DefinitionStatus, ProcessDefinitionRecord, ProcessDefinitionStore};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// In-memory process definition store (register by id, load by id).
pub struct ProcessDefStore {
    defs: RwLock<HashMap<String, ProcessDefinition>>,
    records: RwLock<HashMap<String, ProcessDefinitionRecord>>,
}

impl ProcessDefStore {
    pub fn new() -> Self {
        Self {
            defs: RwLock::new(HashMap::new()),
            records: RwLock::new(HashMap::new()),
        }
    }

    /// Register a compiled process definition. Creates a new version record.
    pub fn register(&self, def: ProcessDefinition) {
        let id = def.id.to_string();
        self.defs.write().unwrap().insert(id.clone(), def);

        // Parse key and version from id (e.g. "order-flow:3" -> key="order-flow", version=3)
        let (key, version) = parse_id_and_version(&id);
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();

        let record = ProcessDefinitionRecord {
            id: id.clone(),
            key: key.clone(),
            version,
            status: DefinitionStatus::Active,
            created_at,
        };

        // Deprecate previous active version for the same key
        {
            let mut records = self.records.write().unwrap();
            for rec in records.values_mut() {
                if rec.key == key && rec.status == DefinitionStatus::Active && rec.id != id {
                    rec.status = DefinitionStatus::Deprecated;
                }
            }
            records.insert(id, record);
        }
    }
}

impl Default for ProcessDefStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse key and version from a definition id like "order-flow:3".
/// If no colon, version defaults to 1.
fn parse_id_and_version(id: &str) -> (String, u32) {
    if let Some((key, ver_str)) = id.rsplit_once(':') {
        if let Ok(v) = ver_str.parse::<u32>() {
            return (key.to_string(), v);
        }
    }
    (id.to_string(), 1)
}

#[async_trait]
impl ProcessDefinitionStore for ProcessDefStore {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessDefinition>> {
        Ok(self.defs.read().unwrap().get(id).cloned())
    }

    async fn list_versions(&self, key: &str) -> anyhow::Result<Vec<ProcessDefinitionRecord>> {
        let records = self.records.read().unwrap();
        let mut versions: Vec<ProcessDefinitionRecord> =
            records.values().filter(|r| r.key == key).cloned().collect();
        versions.sort_by(|a, b| b.version.cmp(&a.version));
        Ok(versions)
    }

    async fn get_active(&self, key: &str) -> anyhow::Result<Option<ProcessDefinitionRecord>> {
        let records = self.records.read().unwrap();
        Ok(records
            .values()
            .filter(|r| r.key == key && r.status == DefinitionStatus::Active)
            .max_by_key(|r| r.version)
            .cloned())
    }

    async fn activate(&self, id: &str) -> anyhow::Result<()> {
        let mut records = self.records.write().unwrap();
        let target = records
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("process definition not found: {}", id))?;
        let key = target.key.clone();

        // Deprecate all other active versions for the same key
        for rec in records.values_mut() {
            if rec.key == key && rec.status == DefinitionStatus::Active && rec.id != id {
                rec.status = DefinitionStatus::Deprecated;
            }
        }
        // Activate the target
        if let Some(rec) = records.get_mut(id) {
            rec.status = DefinitionStatus::Active;
        }
        Ok(())
    }

    async fn deprecate(&self, id: &str) -> anyhow::Result<()> {
        let mut records = self.records.write().unwrap();
        let rec = records
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("process definition not found: {}", id))?;
        rec.status = DefinitionStatus::Deprecated;
        Ok(())
    }
}
