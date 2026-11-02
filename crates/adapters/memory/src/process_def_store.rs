use async_trait::async_trait;
use bpm_engine_core::ProcessDefinition;
use bpm_engine_storage::{
    DefinitionDeployer, DefinitionStatus, ProcessDefinitionRecord, ProcessDefinitionStore,
};
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

    /// Register a compiled process definition under a tenant (N1-3).
    pub fn register(&self, def: ProcessDefinition, tenant_id: Option<String>) {
        let id = def.id.to_string();
        self.defs.write().unwrap().insert(id.clone(), def);

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
            status: DefinitionStatus::Activated,
            created_at,
            tenant_id,
        };

        // Auto-deactivate prior Activated version for the same key AND tenant.
        {
            let mut records = self.records.write().unwrap();
            for rec in records.values_mut() {
                if rec.key == key
                    && rec.status == DefinitionStatus::Activated
                    && rec.id != id
                    && rec.tenant_id == record.tenant_id
                {
                    rec.status = DefinitionStatus::Published;
                }
            }
            records.insert(id, record);
        }
    }

    /// Return metadata for every registered definition version (all tenants).
    pub fn list_all_records(&self) -> Vec<ProcessDefinitionRecord> {
        let mut records: Vec<ProcessDefinitionRecord> =
            self.records.read().unwrap().values().cloned().collect();
        records.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| b.version.cmp(&a.version)));
        records
    }

    /// Filter records by tenant: `None` returns all; `Some(t)` returns
    /// tenant `t`'s records plus global (tenant_id=None) ones (N1-3).
    fn tenant_filter(
        records: Vec<ProcessDefinitionRecord>,
        tenant_id: Option<&str>,
    ) -> Vec<ProcessDefinitionRecord> {
        match tenant_id {
            None => records,
            Some(t) => records
                .into_iter()
                .filter(|r| r.tenant_id.as_deref() == Some(t) || r.tenant_id.is_none())
                .collect(),
        }
    }
}

impl Default for ProcessDefStore {
    fn default() -> Self {
        Self::new()
    }
}

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

    async fn list_versions(
        &self,
        key: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Vec<ProcessDefinitionRecord>> {
        let records = self.records.read().unwrap();
        let mut versions: Vec<ProcessDefinitionRecord> = Self::tenant_filter(
            records.values().filter(|r| r.key == key).cloned().collect(),
            tenant_id,
        );
        versions.sort_by_key(|b| std::cmp::Reverse(b.version));
        Ok(versions)
    }

    async fn get_active(
        &self,
        key: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Option<ProcessDefinitionRecord>> {
        let records = self.records.read().unwrap();
        let candidates: Vec<_> = Self::tenant_filter(
            records
                .values()
                .filter(|r| r.key == key && r.status == DefinitionStatus::Activated)
                .cloned()
                .collect(),
            tenant_id,
        );
        Ok(candidates.into_iter().max_by_key(|r| r.version))
    }

    async fn activate(&self, id: &str) -> anyhow::Result<()> {
        let mut records = self.records.write().unwrap();
        let target = records
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("process definition not found: {}", id))?;
        let key = target.key.clone();
        let tenant = target.tenant_id.clone();

        for rec in records.values_mut() {
            if rec.key == key
                && rec.status == DefinitionStatus::Activated
                && rec.id != id
                && rec.tenant_id == tenant
            {
                rec.status = DefinitionStatus::Published;
            }
        }
        if let Some(rec) = records.get_mut(id) {
            rec.status = DefinitionStatus::Activated;
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

    async fn list_all(
        &self,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Vec<ProcessDefinitionRecord>> {
        let mut records = Self::tenant_filter(self.list_all_records(), tenant_id);
        records.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| b.version.cmp(&a.version)));
        Ok(records)
    }
}

#[async_trait]
impl DefinitionDeployer for ProcessDefStore {
    async fn deploy(
        &self,
        def: &ProcessDefinition,
        _bpmn_xml: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.register(def.clone(), tenant_id.map(String::from));
        Ok(())
    }
}
