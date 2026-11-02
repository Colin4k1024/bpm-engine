//! PostgreSQL-backed ProcessDefinitionStore.
//! Stores BPMN XML and compiles to ProcessDefinition on load.

use async_trait::async_trait;
use bpm_engine_core::ProcessDefinition;
use bpm_engine_storage::{
    DefinitionDeployer, DefinitionStatus, ProcessDefinitionRecord, ProcessDefinitionStore,
};
use deadpool_postgres::Pool;

pub struct PostgresProcessDefStore {
    pool: Pool,
}

impl PostgresProcessDefStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub async fn deploy(
        &self,
        id: &str,
        bpmn_xml: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let (key, version) = parse_id_and_version(id);
        let client = self.pool.get().await?;
        let tenant: Option<String> = tenant_id.map(String::from);
        client
            .execute(
                "INSERT INTO process_definition (id, def_key, version, status, bpmn_xml, created_at, tenant_id)
                 VALUES ($1, $2, $3, 'activated', $4, NOW()::TEXT, $5)
                 ON CONFLICT (id) DO UPDATE SET bpmn_xml = $4",
                &[&id, &key, &version, &bpmn_xml, &tenant],
            )
            .await?;
        // Auto-deactivate prior Activated for the same key AND tenant (→ Published).
        client
            .execute(
                "UPDATE process_definition SET status = 'published'
                 WHERE def_key = $1 AND id != $2 AND status = 'activated'
                 AND (tenant_id IS NOT DISTINCT FROM $3)",
                &[&key, &id, &tenant],
            )
            .await?;
        Ok(())
    }
}

fn parse_id_and_version(id: &str) -> (String, i32) {
    if let Some((key, ver_str)) = id.rsplit_once(':') {
        if let Ok(v) = ver_str.parse::<i32>() {
            return (key.to_string(), v);
        }
    }
    (id.to_string(), 1)
}

fn parse_status(s: &str) -> DefinitionStatus {
    DefinitionStatus::from_db_str(s)
}

fn row_to_record(row: &tokio_postgres::Row) -> ProcessDefinitionRecord {
    ProcessDefinitionRecord {
        id: row.get(0),
        key: row.get(1),
        version: row.get::<_, i32>(2) as u32,
        status: parse_status(row.get(3)),
        created_at: row.get(4),
        tenant_id: row.get(5),
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

    async fn list_versions(
        &self,
        key: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Vec<ProcessDefinitionRecord>> {
        let client = self.pool.get().await?;
        let tenant: Option<String> = tenant_id.map(String::from);
        let rows = client
            .query(
                "SELECT id, def_key, version, status, created_at, tenant_id
                 FROM process_definition
                 WHERE def_key = $1 AND (tenant_id IS NOT DISTINCT FROM $2 OR tenant_id IS NULL)
                 ORDER BY version DESC",
                &[&key, &tenant],
            )
            .await?;
        Ok(rows.iter().map(row_to_record).collect())
    }

    async fn get_active(
        &self,
        key: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Option<ProcessDefinitionRecord>> {
        let client = self.pool.get().await?;
        let tenant: Option<String> = tenant_id.map(String::from);
        let row = client
            .query_opt(
                "SELECT id, def_key, version, status, created_at, tenant_id
                 FROM process_definition
                 WHERE def_key = $1 AND status = 'activated'
                 AND (tenant_id IS NOT DISTINCT FROM $2 OR tenant_id IS NULL)
                 ORDER BY version DESC LIMIT 1",
                &[&key, &tenant],
            )
            .await?;
        Ok(row.map(|row| row_to_record(&row)))
    }

    async fn activate(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT def_key, tenant_id FROM process_definition WHERE id = $1",
                &[&id],
            )
            .await?;
        let (key, tenant): (String, Option<String>) = match row {
            Some(r) => (r.get(0), r.get(1)),
            None => return Err(anyhow::anyhow!("process definition not found: {}", id)),
        };
        client
            .execute(
                "UPDATE process_definition SET status = 'published'
                 WHERE def_key = $1 AND id != $2 AND status = 'activated'
                 AND (tenant_id IS NOT DISTINCT FROM $3)",
                &[&key, &id, &tenant],
            )
            .await?;
        client
            .execute(
                "UPDATE process_definition SET status = 'activated' WHERE id = $1",
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

    async fn list_all(
        &self,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Vec<ProcessDefinitionRecord>> {
        let client = self.pool.get().await?;
        let tenant: Option<String> = tenant_id.map(String::from);
        let rows = client
            .query(
                "SELECT id, def_key, version, status, created_at, tenant_id
                 FROM process_definition
                 WHERE (tenant_id IS NOT DISTINCT FROM $1 OR tenant_id IS NULL)
                 ORDER BY def_key ASC, version DESC",
                &[&tenant],
            )
            .await?;
        Ok(rows.iter().map(row_to_record).collect())
    }
}

#[async_trait]
impl DefinitionDeployer for PostgresProcessDefStore {
    async fn deploy(
        &self,
        def: &ProcessDefinition,
        bpmn_xml: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<()> {
        PostgresProcessDefStore::deploy(self, def.id, bpmn_xml, tenant_id).await
    }
}
