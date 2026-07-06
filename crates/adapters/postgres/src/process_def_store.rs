//! PostgreSQL-backed ProcessDefinitionStore.
//! Stores BPMN XML and compiles to ProcessDefinition on load.

use async_trait::async_trait;
use bpm_engine_core::ProcessDefinition;
use bpm_engine_storage::{DefinitionStatus, ProcessDefinitionRecord, ProcessDefinitionStore};
use deadpool_postgres::Pool;

pub struct PostgresProcessDefStore {
    pool: Pool,
}

impl PostgresProcessDefStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub async fn deploy(&self, id: &str, bpmn_xml: &str) -> anyhow::Result<()> {
        let (key, version) = parse_id_and_version(id);
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO process_definition (id, def_key, version, status, bpmn_xml, created_at)
                 VALUES ($1, $2, $3, 'active', $4, NOW()::TEXT)
                 ON CONFLICT (id) DO UPDATE SET bpmn_xml = $4",
                &[&id, &key, &version, &bpmn_xml],
            )
            .await?;
        // Deprecate previous active version for the same key
        client
            .execute(
                "UPDATE process_definition SET status = 'deprecated'
                 WHERE def_key = $1 AND id != $2 AND status = 'active'",
                &[&key, &id],
            )
            .await?;
        Ok(())
    }
}

/// Parse key and version from a definition id like "order-flow:3".
fn parse_id_and_version(id: &str) -> (String, i32) {
    if let Some((key, ver_str)) = id.rsplit_once(':') {
        if let Ok(v) = ver_str.parse::<i32>() {
            return (key.to_string(), v);
        }
    }
    (id.to_string(), 1)
}

fn parse_status(s: &str) -> DefinitionStatus {
    match s {
        "active" => DefinitionStatus::Active,
        "deprecated" => DefinitionStatus::Deprecated,
        _ => DefinitionStatus::Active,
    }
}

#[async_trait]
impl ProcessDefinitionStore for PostgresProcessDefStore {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessDefinition>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT bpmn_xml FROM process_definition WHERE id = $1",
                &[&id],
            )
            .await?;

        match row {
            None => Ok(None),
            Some(row) => {
                let xml: String = row.get(0);
                let def = bpm_engine_bpmn::parse_and_compile(&xml)
                    .map_err(|e| anyhow::anyhow!("failed to compile definition {id}: {e:?}"))?;
                Ok(Some(def))
            }
        }
    }

    async fn list_versions(&self, key: &str) -> anyhow::Result<Vec<ProcessDefinitionRecord>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, def_key, version, status, created_at
                 FROM process_definition WHERE def_key = $1 ORDER BY version DESC",
                &[&key],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| ProcessDefinitionRecord {
                id: row.get(0),
                key: row.get(1),
                version: row.get::<_, i32>(2) as u32,
                status: parse_status(row.get(3)),
                created_at: row.get(4),
            })
            .collect())
    }

    async fn get_active(&self, key: &str) -> anyhow::Result<Option<ProcessDefinitionRecord>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, def_key, version, status, created_at
                 FROM process_definition WHERE def_key = $1 AND status = 'active'
                 ORDER BY version DESC LIMIT 1",
                &[&key],
            )
            .await?;
        Ok(row.map(|row| ProcessDefinitionRecord {
            id: row.get(0),
            key: row.get(1),
            version: row.get::<_, i32>(2) as u32,
            status: parse_status(row.get(3)),
            created_at: row.get(4),
        }))
    }

    async fn activate(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        // Get the key for this definition
        let row = client
            .query_opt(
                "SELECT def_key FROM process_definition WHERE id = $1",
                &[&id],
            )
            .await?;
        let key: String = match row {
            Some(r) => r.get(0),
            None => return Err(anyhow::anyhow!("process definition not found: {}", id)),
        };
        // Deprecate all other active versions for the same key
        client
            .execute(
                "UPDATE process_definition SET status = 'deprecated'
                 WHERE def_key = $1 AND id != $2 AND status = 'active'",
                &[&key, &id],
            )
            .await?;
        // Activate the target
        client
            .execute(
                "UPDATE process_definition SET status = 'active' WHERE id = $1",
                &[&id],
            )
            .await?;
        Ok(())
    }

    async fn deprecate(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        let rows_affected = client
            .execute(
                "UPDATE process_definition SET status = 'deprecated' WHERE id = $1",
                &[&id],
            )
            .await?;
        if rows_affected == 0 {
            return Err(anyhow::anyhow!("process definition not found: {}", id));
        }
        Ok(())
    }
}
