use async_trait::async_trait;
use bpm_engine_storage::{OutboxEvent, OutboxRepo};
use deadpool_postgres::Pool;

/// PostgreSQL implementation of OutboxRepo (transactional outbox pattern).
pub struct PostgresOutboxRepo {
    pool: Pool,
}

impl PostgresOutboxRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OutboxRepo for PostgresOutboxRepo {
    async fn insert_pending(
        &self,
        tenant_id: Option<&str>,
        event_type: &str,
        payload: &str,
    ) -> anyhow::Result<String> {
        let client = self.pool.get().await?;
        let id = uuid::Uuid::new_v4().to_string();

        client
            .execute(
                r#"
                INSERT INTO event_outbox (id, tenant_id, event_type, payload, status, created_at)
                VALUES ($1, $2, $3, $4, 'Pending', CURRENT_TIMESTAMP)
                "#,
                &[&id, &tenant_id, &event_type, &payload],
            )
            .await?;

        Ok(id)
    }

    async fn list_pending(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<OutboxEvent>> {
        let client = self.pool.get().await?;

        let rows = match tenant_id {
            Some(t) => {
                client
                    .query(
                        r#"
                        SELECT id, tenant_id, event_type, payload, status, created_at
                        FROM event_outbox
                        WHERE status = 'Pending' AND (tenant_id = $1 OR (tenant_id IS NULL AND $1 = ''))
                        ORDER BY created_at ASC
                        LIMIT 100
                        "#,
                        &[&t],
                    )
                    .await?
            }
            None => {
                client
                    .query(
                        r#"
                        SELECT id, tenant_id, event_type, payload, status, created_at
                        FROM event_outbox
                        WHERE status = 'Pending'
                        ORDER BY created_at ASC
                        LIMIT 100
                        "#,
                        &[],
                    )
                    .await?
            }
        };

        Ok(rows
            .iter()
            .map(|r| OutboxEvent {
                id: r.get("id"),
                tenant_id: r.get("tenant_id"),
                event_type: r.get("event_type"),
                payload: r.get("payload"),
                status: r.get("status"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn mark_published(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                UPDATE event_outbox SET status = 'Published' WHERE id = $1
                "#,
                &[&id],
            )
            .await?;
        Ok(())
    }

    async fn claim_pending(
        &self,
        worker_id: &str,
        tenant_id: Option<&str>,
        limit: u32,
    ) -> anyhow::Result<Vec<OutboxEvent>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        // Reclaim stale claimed events
        tx.execute(
            r#"
            UPDATE event_outbox
            SET status = 'Pending', claimed_by = NULL
            WHERE status = 'Dispatched' AND claimed_by = $1
            "#,
            &[&worker_id],
        )
        .await?;

        // Claim new pending events
        let rows = match tenant_id {
            Some(t) => {
                tx.query(
                    r#"
                    UPDATE event_outbox
                    SET status = 'Dispatched', claimed_by = $1
                    WHERE id IN (
                        SELECT id FROM event_outbox
                        WHERE status = 'Pending'
                          AND (tenant_id = $2 OR (tenant_id IS NULL AND $2 = ''))
                        ORDER BY created_at ASC
                        LIMIT $3
                        FOR UPDATE SKIP LOCKED
                    )
                    RETURNING id, tenant_id, event_type, payload, status, created_at
                    "#,
                    &[&worker_id, &t, &(limit as i64)],
                )
                .await?
            }
            None => {
                tx.query(
                    r#"
                    UPDATE event_outbox
                    SET status = 'Dispatched', claimed_by = $1
                    WHERE id IN (
                        SELECT id FROM event_outbox
                        WHERE status = 'Pending'
                        ORDER BY created_at ASC
                        LIMIT $2
                        FOR UPDATE SKIP LOCKED
                    )
                    RETURNING id, tenant_id, event_type, payload, status, created_at
                    "#,
                    &[&worker_id, &(limit as i64)],
                )
                .await?
            }
        };

        tx.commit().await?;

        Ok(rows
            .iter()
            .map(|r| OutboxEvent {
                id: r.get("id"),
                tenant_id: r.get("tenant_id"),
                event_type: r.get("event_type"),
                payload: r.get("payload"),
                status: r.get("status"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn release_claimed(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                UPDATE event_outbox
                SET status = 'Pending', claimed_by = NULL
                WHERE id = $1 AND status = 'Dispatched'
                "#,
                &[&id],
            )
            .await?;
        Ok(())
    }
}
