//! PostgreSQL-backed DefinitionAuditStore (N1-4).
//!
//! Stores audit records for definition lifecycle operations (deploy,
//! activate, deprecate, retire) in the `definition_audit` table (migration
//! 0008). Survives restarts in postgres mode.

use async_trait::async_trait;
use bpm_engine_storage::{DefinitionAuditOperation, DefinitionAuditRecord, DefinitionAuditStore};
use deadpool_postgres::Pool;

pub struct PostgresDefinitionAuditStore {
    pool: Pool,
}

impl PostgresDefinitionAuditStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DefinitionAuditStore for PostgresDefinitionAuditStore {
    async fn append(&self, record: DefinitionAuditRecord) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO definition_audit
                 (definition_id, definition_key, version, operation, actor,
                  content_hash, content_size, reason, timestamp)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &record.definition_id,
                    &record.definition_key,
                    &(record.version as i32),
                    &record.operation.to_string(),
                    &record.actor,
                    &record.content_hash,
                    &(record.content_size.map(|v| v as i64)),
                    &record.reason,
                    &record.timestamp,
                ],
            )
            .await?;
        Ok(())
    }

    async fn list_by_definition(
        &self,
        definition_id: &str,
    ) -> anyhow::Result<Vec<DefinitionAuditRecord>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT definition_id, definition_key, version, operation, actor,
                        content_hash, content_size, reason, timestamp
                 FROM definition_audit
                 WHERE definition_id = $1
                 ORDER BY id ASC",
                &[&definition_id],
            )
            .await?;
        Ok(rows.iter().map(row_to_record).collect())
    }

    async fn list_by_key(&self, key: &str) -> anyhow::Result<Vec<DefinitionAuditRecord>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT definition_id, definition_key, version, operation, actor,
                        content_hash, content_size, reason, timestamp
                 FROM definition_audit
                 WHERE definition_key = $1
                 ORDER BY id ASC",
                &[&key],
            )
            .await?;
        Ok(rows.iter().map(row_to_record).collect())
    }
}

fn row_to_record(row: &tokio_postgres::Row) -> DefinitionAuditRecord {
    let op_str: String = row.get(3);
    let operation = match op_str.as_str() {
        "deploy" => DefinitionAuditOperation::Deploy,
        "activate" => DefinitionAuditOperation::Activate,
        "deactivate" => DefinitionAuditOperation::Deactivate,
        "deprecate" => DefinitionAuditOperation::Deprecate,
        "retire" => DefinitionAuditOperation::Retire,
        _ => DefinitionAuditOperation::Deploy, // fallback
    };
    let content_size: Option<i64> = row.get(6);
    DefinitionAuditRecord {
        sequence: 0, // sequence is the BIGSERIAL id, not surfaced in the record
        definition_id: row.get(0),
        definition_key: row.get(1),
        version: row.get::<_, i32>(2) as u32,
        operation,
        actor: row.get(4),
        content_hash: row.get(5),
        content_size: content_size.map(|v| v as u64),
        reason: row.get(7),
        timestamp: row.get(8),
    }
}
