use async_trait::async_trait;
use bpm_engine_core::{ApprovalDecision, ApprovalEffect, ApprovalRecord, ApprovalStatus};
use bpm_engine_storage::{ApprovalDecideOutcome, ApprovalError, ApprovalStore};
use deadpool_postgres::Pool;

/// PostgreSQL implementation of ApprovalStore (E5-2, ADR-008).
///
/// Backed by the `approval_record` table (migration 0004). All statements
/// are parameter-bound; only-once semantics come from conditional SQL:
/// `create` upserts with `ON CONFLICT (instance_id, node_id, token_id) DO
/// NOTHING`, and `decide` updates `WHERE ... status = 'Pending'` — 0
/// updated rows fall through to a read-back that classifies
/// `NotFound` vs `AlreadyDecided` with the original outcome.
///
/// Decision vs. effect (ADR-008 §7.1): `decide` commits the DECISION with
/// `effect = 'Pending'`; the caller applies the effect and confirms it
/// with [`ApprovalStore::mark_effect_applied`]. Rows with a decided status
/// and a pending effect are re-driven by a retried decide and by the
/// server's startup sweep
/// ([`ApprovalStore::list_decided_with_pending_effect`]).
pub struct PostgresApprovalStore {
    pool: Pool,
}

impl PostgresApprovalStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

fn row_to_record(row: &tokio_postgres::Row) -> ApprovalRecord {
    let status: String = row.get("status");
    let effect: String = row.get("effect");
    ApprovalRecord {
        id: row.get("id"),
        instance_id: row.get("instance_id"),
        node_id: row.get("node_id"),
        token_id: row.get("token_id"),
        task_type: row.get("task_type"),
        status: ApprovalStatus::parse(&status).unwrap_or(ApprovalStatus::Pending),
        effect: ApprovalEffect::parse(&effect).unwrap_or(ApprovalEffect::Pending),
        decided_by: row.get("decided_by"),
        decided_at: row.get("decided_at"),
        note: row.get("note"),
        created_at: row
            .get::<_, Option<String>>("created_at")
            .unwrap_or_default(),
    }
}

const SELECT_BY_ID: &str = r#"
    SELECT id, instance_id, node_id, token_id, task_type, status,
           effect, decided_by, decided_at, note, created_at
    FROM approval_record
    WHERE id = $1
"#;

#[async_trait]
impl ApprovalStore for PostgresApprovalStore {
    async fn create(&self, record: &ApprovalRecord) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        // Idempotent by subject: ON CONFLICT DO NOTHING keeps the original
        // record (id, status, decision) when the same token re-arrives at
        // the same node.
        client
            .execute(
                r#"
                INSERT INTO approval_record
                    (id, instance_id, node_id, token_id, task_type, status,
                     effect, decided_by, decided_at, note, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                ON CONFLICT (instance_id, node_id, token_id) DO NOTHING
                "#,
                &[
                    &record.id,
                    &record.instance_id,
                    &record.node_id,
                    &record.token_id,
                    &record.task_type,
                    &record.status.as_str(),
                    &record.effect.as_str(),
                    &record.decided_by,
                    &record.decided_at,
                    &record.note,
                    &record.created_at,
                ],
            )
            .await?;
        Ok(())
    }

    async fn get(&self, approval_id: &str) -> anyhow::Result<Option<ApprovalRecord>> {
        let client = self.pool.get().await?;
        let row = client.query_opt(SELECT_BY_ID, &[&approval_id]).await?;
        Ok(row.as_ref().map(row_to_record))
    }

    async fn list_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<ApprovalRecord>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                r#"
                SELECT id, instance_id, node_id, token_id, task_type, status,
                       effect, decided_by, decided_at, note, created_at
                FROM approval_record
                WHERE instance_id = $1
                ORDER BY created_at ASC, id ASC
                "#,
                &[&instance_id],
            )
            .await?;
        Ok(rows.iter().map(row_to_record).collect())
    }

    async fn decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
    ) -> Result<ApprovalDecideOutcome, ApprovalError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| ApprovalError::Internal(e.to_string()))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();
        let new_status = decision.resulting_status().as_str();

        // Atomic only-once: the conditional UPDATE (status = 'Pending')
        // lets exactly ONE decide flip the row — a concurrent second
        // transaction blocks on the row lock, re-evaluates the predicate,
        // and updates 0 rows.
        //
        // The UPDATE also (re)sets effect = 'Pending': the DECISION commits
        // durable here, its EFFECT is applied by the caller afterwards and
        // confirmed via mark_effect_applied (ADR-008 §7.1).
        let rows = client
            .query(
                r#"
                UPDATE approval_record
                SET status = $2, effect = 'Pending', decided_by = $3,
                    decided_at = $4, note = $5
                WHERE id = $1 AND status = 'Pending'
                RETURNING id, instance_id, node_id, token_id, task_type,
                          status, effect, decided_by, decided_at, note,
                          created_at
                "#,
                &[&approval_id, &new_status, &actor, &now, &note],
            )
            .await
            .map_err(|e| ApprovalError::Internal(e.to_string()))?;
        if let Some(row) = rows.first() {
            return Ok(ApprovalDecideOutcome::Decided(row_to_record(row)));
        }

        // 0 rows: unknown id, or already decided — read back to classify.
        let row = client
            .query_opt(SELECT_BY_ID, &[&approval_id])
            .await
            .map_err(|e| ApprovalError::Internal(e.to_string()))?;
        match row {
            Some(row) => Ok(ApprovalDecideOutcome::AlreadyDecided(row_to_record(&row))),
            None => Err(ApprovalError::NotFound {
                approval_id: approval_id.to_string(),
            }),
        }
    }

    async fn mark_effect_applied(&self, approval_id: &str) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        // Idempotent confirmation: updating an already-applied row is the
        // same write; an unknown id updates 0 rows (the caller loaded the
        // record first) and is tolerated.
        client
            .execute(
                "UPDATE approval_record SET effect = 'Applied' WHERE id = $1",
                &[&approval_id],
            )
            .await?;
        Ok(())
    }

    async fn list_decided_with_pending_effect(&self) -> anyhow::Result<Vec<ApprovalRecord>> {
        // The startup sweep's work list (ADR-008 §7.1): durable decisions
        // whose effect has not (fully) been applied, in creation order.
        let client = self.pool.get().await?;
        let rows = client
            .query(
                r#"
                SELECT id, instance_id, node_id, token_id, task_type, status,
                       effect, decided_by, decided_at, note, created_at
                FROM approval_record
                WHERE status <> 'Pending' AND effect = 'Pending'
                ORDER BY created_at ASC, id ASC
                "#,
                &[],
            )
            .await?;
        Ok(rows.iter().map(row_to_record).collect())
    }
}
