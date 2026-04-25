use async_trait::async_trait;
use bpm_engine_storage::{HistoryEvent, HistoryRepo};
use deadpool_postgres::Pool;

/// PostgreSQL implementation of HistoryRepo.
pub struct PostgresHistoryRepo {
    pool: Pool,
}

impl PostgresHistoryRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HistoryRepo for PostgresHistoryRepo {
    async fn append(
        &self,
        instance_id: &str,
        event_type: &str,
        payload: &serde_json::Value,
        occurred_at: &str,
    ) -> anyhow::Result<String> {
        let client = self.pool.get().await?;
        let id = uuid::Uuid::new_v4().to_string();

        client
            .execute(
                r#"
                INSERT INTO history_event (id, instance_id, event_type, payload, occurred_at)
                VALUES ($1, $2, $3, $4, $5)
                "#,
                &[
                    &id,
                    &instance_id,
                    &event_type,
                    &serde_json::to_string(payload)?,
                    &occurred_at,
                ],
            )
            .await?;

        Ok(id)
    }

    async fn list_by_instance(
        &self,
        instance_id: &str,
        token_id_filter: Option<&str>,
        event_type_filter: Option<&str>,
    ) -> anyhow::Result<Vec<HistoryEvent>> {
        let client = self.pool.get().await?;

        let rows = match (token_id_filter, event_type_filter) {
            (Some(tid), Some(et)) => {
                client
                    .query(
                        r#"
                        SELECT id, instance_id, event_type, payload, occurred_at
                        FROM history_event
                        WHERE instance_id = $1 AND payload->>'token_id' = $2 AND event_type = $3
                        ORDER BY occurred_at ASC
                        "#,
                        &[&instance_id, &tid, &et],
                    )
                    .await?
            }
            (Some(tid), None) => {
                client
                    .query(
                        r#"
                        SELECT id, instance_id, event_type, payload, occurred_at
                        FROM history_event
                        WHERE instance_id = $1 AND payload->>'token_id' = $2
                        ORDER BY occurred_at ASC
                        "#,
                        &[&instance_id, &tid],
                    )
                    .await?
            }
            (None, Some(et)) => {
                client
                    .query(
                        r#"
                        SELECT id, instance_id, event_type, payload, occurred_at
                        FROM history_event
                        WHERE instance_id = $1 AND event_type = $2
                        ORDER BY occurred_at ASC
                        "#,
                        &[&instance_id, &et],
                    )
                    .await?
            }
            (None, None) => {
                client
                    .query(
                        r#"
                        SELECT id, instance_id, event_type, payload, occurred_at
                        FROM history_event
                        WHERE instance_id = $1
                        ORDER BY occurred_at ASC
                        "#,
                        &[&instance_id],
                    )
                    .await?
            }
        };

        Ok(rows
            .iter()
            .map(|r| {
                let payload_str: String = r.get("payload");
                let payload: serde_json::Value =
                    serde_json::from_str(&payload_str).unwrap_or(serde_json::json!({}));
                HistoryEvent {
                    id: r.get("id"),
                    instance_id: r.get("instance_id"),
                    event_type: r.get("event_type"),
                    payload,
                    occurred_at: r.get("occurred_at"),
                }
            })
            .collect())
    }
}
