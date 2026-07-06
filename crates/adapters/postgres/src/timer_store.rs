use async_trait::async_trait;
use bpm_engine_storage::{TimerRecord, TimerStore};
use deadpool_postgres::Pool;

/// PostgreSQL implementation of TimerStore.
pub struct PostgresTimerStore {
    pool: Pool,
}

impl PostgresTimerStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TimerStore for PostgresTimerStore {
    async fn get_by_id(&self, id: &str) -> anyhow::Result<Option<TimerRecord>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                r#"
                SELECT id, token_id, instance_id, node_id, due_at, status, created_at
                FROM timer
                WHERE id = $1
                "#,
                &[&id],
            )
            .await?;

        Ok(row.map(|r| TimerRecord {
            id: r.get("id"),
            token_id: r.get("token_id"),
            instance_id: r.get("instance_id"),
            node_id: r.get("node_id"),
            due_at: r.get("due_at"),
            status: r.get("status"),
            created_at: r.get("created_at"),
        }))
    }

    async fn mark_fired(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                UPDATE timer SET status = 'Fired' WHERE id = $1
                "#,
                &[&id],
            )
            .await?;
        Ok(())
    }

    async fn insert(&self, record: &TimerRecord) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                INSERT INTO timer (id, token_id, instance_id, node_id, due_at, status, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (id) DO UPDATE SET
                    due_at = EXCLUDED.due_at,
                    status = EXCLUDED.status
                "#,
                &[
                    &record.id,
                    &record.token_id,
                    &record.instance_id,
                    &record.node_id,
                    &record.due_at,
                    &record.status,
                    &record.created_at,
                ],
            )
            .await?;
        Ok(())
    }

    async fn list_due(&self, now_iso: &str, limit: u32) -> anyhow::Result<Vec<TimerRecord>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                r#"
                SELECT id, token_id, instance_id, node_id, due_at, status, created_at
                FROM timer
                WHERE status = 'Scheduled' AND due_at <= $1
                ORDER BY due_at ASC
                LIMIT $2
                "#,
                &[&now_iso, &(limit as i64)],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|r| TimerRecord {
                id: r.get("id"),
                token_id: r.get("token_id"),
                instance_id: r.get("instance_id"),
                node_id: r.get("node_id"),
                due_at: r.get("due_at"),
                status: r.get("status"),
                created_at: r.get("created_at"),
            })
            .collect())
    }
}
