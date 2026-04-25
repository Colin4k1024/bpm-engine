use async_trait::async_trait;
use bpm_engine_storage::{CompensationRecordRepo, CompensationRecordRow};
use deadpool_postgres::Pool;

/// PostgreSQL implementation of CompensationRecordRepo.
pub struct PostgresCompensationRepo {
    pool: Pool,
}

impl PostgresCompensationRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CompensationRecordRepo for PostgresCompensationRepo {
    async fn add(&self, record: &CompensationRecordRow) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                INSERT INTO compensation_record
                    (id, instance_id, node_id, handler_ref, order, status, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
                &[
                    &record.id,
                    &record.instance_id,
                    &record.node_id,
                    &record.handler_ref,
                    &(record.order as i32),
                    &record.status,
                    &record.created_at,
                ],
            )
            .await?;
        Ok(())
    }

    async fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow> {
        let client = match self.pool.get().await {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let rows = match client
            .query(
                r#"
                SELECT id, instance_id, node_id, handler_ref, order, status, created_at
                FROM compensation_record
                WHERE instance_id = $1
                ORDER BY order ASC
                "#,
                &[&instance_id],
            )
            .await
        {
            Ok(r) => r,
            Err(_) => return vec![],
        };

        rows.iter()
            .map(|r| CompensationRecordRow {
                id: r.get("id"),
                instance_id: r.get("instance_id"),
                node_id: r.get("node_id"),
                handler_ref: r.get("handler_ref"),
                order: r.get::<_, i32>("order") as u32,
                status: r.get("status"),
                created_at: r.get("created_at"),
            })
            .collect()
    }
}
