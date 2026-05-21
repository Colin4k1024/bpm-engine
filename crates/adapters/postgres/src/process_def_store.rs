//! PostgreSQL-backed ProcessDefinitionStore.
//! Stores BPMN XML and compiles to ProcessDefinition on load.

use async_trait::async_trait;
use bpm_engine_core::ProcessDefinition;
use bpm_engine_storage::ProcessDefinitionStore;
use deadpool_postgres::Pool;

pub struct PostgresProcessDefStore {
    pool: Pool,
}

impl PostgresProcessDefStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub async fn deploy(&self, id: &str, bpmn_xml: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO process_definition (id, bpmn_xml, created_at)
                 VALUES ($1, $2, NOW()::TEXT)
                 ON CONFLICT (id) DO UPDATE SET bpmn_xml = $2",
                &[&id, &bpmn_xml],
            )
            .await?;
        Ok(())
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
}
