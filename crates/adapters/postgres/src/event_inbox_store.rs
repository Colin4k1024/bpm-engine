use async_trait::async_trait;
use bpm_engine_storage::{DeliveryStatus, DeliveryType, EventInboxStore, PendingDelivery};
use deadpool_postgres::Pool;

/// PostgreSQL implementation of EventInboxStore (N1-2, ADR-010).
///
/// Backed by the `event_inbox` table (migration 0007). All statements use
/// parameter binding (`$1`, `$2`, ...). Idempotency comes from
/// `INSERT ... ON CONFLICT (dedup_key) DO NOTHING`. JSONB columns
/// (`correlation`, `payload`) are written as JSON text cast to `jsonb` and
/// read back via `::text` so no `with-serde_json-1` feature is required.
pub struct PostgresEventInboxStore {
    pool: Pool,
}

impl PostgresEventInboxStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

fn delivery_type_to_str(dt: DeliveryType) -> &'static str {
    match dt {
        DeliveryType::Message => "message",
        DeliveryType::Signal => "signal",
    }
}

fn str_to_delivery_type(s: &str) -> DeliveryType {
    match s {
        "signal" => DeliveryType::Signal,
        _ => DeliveryType::Message,
    }
}

fn delivery_status_to_str(ds: DeliveryStatus) -> &'static str {
    match ds {
        DeliveryStatus::Pending => "pending",
        DeliveryStatus::Consumed => "consumed",
        DeliveryStatus::Expired => "expired",
    }
}

fn str_to_delivery_status(s: &str) -> DeliveryStatus {
    match s {
        "consumed" => DeliveryStatus::Consumed,
        "expired" => DeliveryStatus::Expired,
        _ => DeliveryStatus::Pending,
    }
}

/// Map a `tokio_postgres::Row` to a `PendingDelivery`. JSONB columns are read
/// as text (`::text` in the SELECT) and deserialized with `serde_json`.
fn row_to_delivery(row: &tokio_postgres::Row) -> PendingDelivery {
    let delivery_type: String = row.get("delivery_type");
    let status: String = row.get("status");
    let correlation: String = row.get("correlation");
    let payload: String = row.get("payload");
    PendingDelivery {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        target_instance_id: row.get("target_instance_id"),
        delivery_type: str_to_delivery_type(&delivery_type),
        name: row.get("name"),
        correlation: serde_json::from_str(&correlation).unwrap_or_default(),
        payload: serde_json::from_str(&payload).unwrap_or_default(),
        status: str_to_delivery_status(&status),
        dedup_key: row.get("dedup_key"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }
}

const SELECT_COLUMNS: &str = r#"
    SELECT id, tenant_id, target_instance_id, delivery_type, name,
           correlation::text, payload::text, status, dedup_key,
           created_at, expires_at
    FROM event_inbox
"#;

#[async_trait]
impl EventInboxStore for PostgresEventInboxStore {
    async fn insert(&self, record: &PendingDelivery) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        let correlation_json = serde_json::to_string(&record.correlation)?;
        let payload_json = serde_json::to_string(&record.payload)?;
        // Idempotent by dedup_key: ON CONFLICT DO NOTHING keeps the original
        // record when the same delivery is re-inserted.
        client
            .execute(
                r#"
                INSERT INTO event_inbox
                    (id, tenant_id, target_instance_id, delivery_type, name,
                     correlation, payload, status, dedup_key, created_at, expires_at)
                VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7::jsonb, $8, $9, $10, $11)
                ON CONFLICT (dedup_key) DO NOTHING
                "#,
                &[
                    &record.id,
                    &record.tenant_id,
                    &record.target_instance_id,
                    &delivery_type_to_str(record.delivery_type),
                    &record.name,
                    &correlation_json,
                    &payload_json,
                    &delivery_status_to_str(record.status),
                    &record.dedup_key,
                    &record.created_at,
                    &record.expires_at,
                ],
            )
            .await?;
        Ok(())
    }

    async fn list_pending(
        &self,
        tenant_id: Option<&str>,
        name: &str,
        target_instance_id: Option<&str>,
    ) -> anyhow::Result<Vec<PendingDelivery>> {
        let client = self.pool.get().await?;
        // A specific tenant sees its own deliveries plus global (no-tenant)
        // ones; a signal broadcast (no target_instance_id) is visible to any
        // instance query. IS NOT DISTINCT FROM handles NULL equality.
        let rows = client
            .query(
                &format!(
                    "{SELECT_COLUMNS}
                    WHERE status = 'pending'
                      AND name = $1
                      AND (tenant_id IS NOT DISTINCT FROM $2 OR tenant_id IS NULL)
                      AND (target_instance_id IS NOT DISTINCT FROM $3 OR target_instance_id IS NULL)
                    ORDER BY created_at ASC"
                ),
                &[&name, &tenant_id, &target_instance_id],
            )
            .await?;
        Ok(rows.iter().map(row_to_delivery).collect())
    }

    async fn list_pending_for_instance(
        &self,
        instance_id: &str,
    ) -> anyhow::Result<Vec<PendingDelivery>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                &format!(
                    "{SELECT_COLUMNS}
                    WHERE target_instance_id = $1 AND status = 'pending'
                    ORDER BY created_at ASC"
                ),
                &[&instance_id],
            )
            .await?;
        Ok(rows.iter().map(row_to_delivery).collect())
    }

    async fn mark_consumed(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        // Idempotent: updating an already-consumed row is the same write; an
        // unknown id updates 0 rows and is tolerated.
        client
            .execute(
                r#"
                UPDATE event_inbox SET status = 'consumed' WHERE id = $1
                "#,
                &[&id],
            )
            .await?;
        Ok(())
    }

    async fn mark_expired(&self, id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                UPDATE event_inbox SET status = 'expired' WHERE id = $1
                "#,
                &[&id],
            )
            .await?;
        Ok(())
    }

    async fn list_expired(&self, now: &str, limit: u32) -> anyhow::Result<Vec<PendingDelivery>> {
        let client = self.pool.get().await?;
        // Sweeper: pending deliveries whose expires_at has passed. String
        // comparison matches the TEXT epoch/ISO convention used elsewhere.
        let rows = client
            .query(
                &format!(
                    "{SELECT_COLUMNS}
                    WHERE status = 'pending'
                      AND expires_at IS NOT NULL
                      AND expires_at < $1
                    ORDER BY created_at ASC
                    LIMIT $2"
                ),
                &[&now, &(limit as i64)],
            )
            .await?;
        Ok(rows.iter().map(row_to_delivery).collect())
    }
}
