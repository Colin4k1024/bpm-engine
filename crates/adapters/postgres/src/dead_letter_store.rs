//! PostgreSQL implementation of DeadLetterStore.

use async_trait::async_trait;
use bpm_engine_storage::{DeadLetterEntry, DeadLetterStore};
use deadpool_postgres::Pool;

/// PostgreSQL dead letter queue store.
pub struct PostgresDeadLetterStore {
    pool: Pool,
}

impl PostgresDeadLetterStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DeadLetterStore for PostgresDeadLetterStore {
    async fn insert(&self, entry: &DeadLetterEntry) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                INSERT INTO dead_letter (id, task_id, token_id, process_instance_id, task_type, error_message, variables, tenant_id, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (id) DO NOTHING
                "#,
                &[
                    &entry.id,
                    &entry.task_id,
                    &entry.token_id,
                    &entry.process_instance_id,
                    &entry.task_type,
                    &entry.error_message,
                    &entry.variables,
                    &entry.tenant_id,
                    &entry.created_at,
                ],
            )
            .await?;
        Ok(())
    }

    async fn list(
        &self,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<DeadLetterEntry>> {
        let client = self.pool.get().await?;
        let rows = if let Some(tid) = tenant_id {
            client
                .query(
                    r#"
                    SELECT id, task_id, token_id, process_instance_id, task_type,
                           error_message, variables, tenant_id, created_at
                    FROM dead_letter
                    WHERE tenant_id = $1
                    ORDER BY created_at DESC
                    LIMIT $2
                    "#,
                    &[&tid, &(limit as i64)],
                )
                .await?
        } else {
            client
                .query(
                    r#"
                    SELECT id, task_id, token_id, process_instance_id, task_type,
                           error_message, variables, tenant_id, created_at
                    FROM dead_letter
                    ORDER BY created_at DESC
                    LIMIT $1
                    "#,
                    &[&(limit as i64)],
                )
                .await?
        };

        Ok(rows
            .iter()
            .map(|r| DeadLetterEntry {
                id: r.get("id"),
                task_id: r.get("task_id"),
                token_id: r.get("token_id"),
                process_instance_id: r.get("process_instance_id"),
                task_type: r.get("task_type"),
                error_message: r.get("error_message"),
                variables: r.get("variables"),
                tenant_id: r.get("tenant_id"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<DeadLetterEntry>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                r#"
                SELECT id, task_id, token_id, process_instance_id, task_type,
                       error_message, variables, tenant_id, created_at
                FROM dead_letter
                WHERE id = $1
                "#,
                &[&id],
            )
            .await?;

        Ok(row.map(|r| DeadLetterEntry {
            id: r.get("id"),
            task_id: r.get("task_id"),
            token_id: r.get("token_id"),
            process_instance_id: r.get("process_instance_id"),
            task_type: r.get("task_type"),
            error_message: r.get("error_message"),
            variables: r.get("variables"),
            tenant_id: r.get("tenant_id"),
            created_at: r.get("created_at"),
        }))
    }

    async fn requeue(&self, id: &str) -> anyhow::Result<Option<String>> {
        let client = self.pool.get().await?;
        // Atomically delete and return the task_id
        let row = client
            .query_opt(
                "DELETE FROM dead_letter WHERE id = $1 RETURNING task_id",
                &[&id],
            )
            .await?;
        Ok(row.map(|r| r.get("task_id")))
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute("DELETE FROM dead_letter WHERE id = $1", &[&id])
            .await?;
        Ok(())
    }
}
