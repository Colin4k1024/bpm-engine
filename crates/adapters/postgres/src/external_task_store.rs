use async_trait::async_trait;
use bpm_engine_core::{ExternalTask, ExternalTaskState};
use bpm_engine_storage::ExternalTaskStore;
use deadpool_postgres::Pool;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// PostgreSQL implementation of ExternalTaskStore.
pub struct PostgresExternalTaskStore {
    pool: Pool,
}

impl PostgresExternalTaskStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

fn str_to_external_task_state(s: &str) -> ExternalTaskState {
    match s {
        "Locked" => ExternalTaskState::Locked,
        "Completed" => ExternalTaskState::Completed,
        "Failed" => ExternalTaskState::Failed,
        _ => ExternalTaskState::Ready,
    }
}

/// Current unix epoch seconds (TEXT epoch-second convention used by the
/// `lock_expire_at` / `updated_at` columns).
fn unix_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[async_trait]
impl ExternalTaskStore for PostgresExternalTaskStore {
    async fn create(
        &self,
        token_id: &str,
        process_instance_id: &str,
        task_type: &str,
        retries: i32,
        timeout_secs: u64,
        variables: HashMap<String, String>,
    ) -> anyhow::Result<String> {
        let client = self.pool.get().await?;
        let task_id = uuid::Uuid::new_v4().to_string();

        // lock_expire_at is TEXT unix-epoch seconds (same convention as the
        // memory adapter and extend_lock below).
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let lock_expire_at = now + timeout_secs;

        client
            .execute(
                r#"
                INSERT INTO external_task
                    (id, token_id, process_instance_id, task_type, retries, timeout_secs,
                     variables, state, lock_expire_at, worker_id, error_message, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, 'Ready',
                        $8, NULL, NULL, $9)
                "#,
                &[
                    &task_id,
                    &token_id,
                    &process_instance_id,
                    &task_type,
                    &retries,
                    &(timeout_secs as i32),
                    &serde_json::to_string(&variables)?,
                    &lock_expire_at.to_string(),
                    &now.to_string(),
                ],
            )
            .await?;

        Ok(task_id)
    }

    async fn fetch_and_lock(
        &self,
        worker_id: &str,
        task_types: &[String],
        max_tasks: usize,
        lock_duration: Duration,
    ) -> anyhow::Result<Vec<ExternalTask>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        // Reclaim expired locks first (lock_expire_at is TEXT unix-epoch
        // seconds; the comparison is lexicographic, which matches numeric
        // order for the fixed-width epoch strings the engine produces).
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        tx.execute(
            r#"
            UPDATE external_task
            SET state = 'Ready', lock_expire_at = NULL, worker_id = NULL
            WHERE state = 'Locked' AND lock_expire_at < $1
            "#,
            &[&now.to_string()],
        )
        .await?;

        // Fetch and lock tasks atomically
        let lock_expire_at = now + lock_duration.as_secs();

        let task_types_str: Vec<&str> = task_types.iter().map(|s| s.as_str()).collect();
        let rows = tx
            .query(
                r#"
                UPDATE external_task
                SET state = 'Locked',
                    worker_id = $1,
                    lock_expire_at = $2,
                    version = version + 1
                WHERE id IN (
                    SELECT id FROM external_task
                    WHERE state = 'Ready'
                      AND task_type = ANY($3)
                    ORDER BY created_at ASC
                    LIMIT $4
                    FOR UPDATE SKIP LOCKED
                )
                RETURNING id, token_id, process_instance_id, task_type, retries,
                          timeout_secs, variables, state, worker_id, error_message,
                          lock_expire_at, created_at, updated_at
                "#,
                &[
                    &worker_id,
                    &lock_expire_at.to_string(),
                    &task_types_str,
                    &(max_tasks as i64),
                ],
            )
            .await?;

        tx.commit().await?;

        let tasks: Vec<ExternalTask> = rows
            .iter()
            .map(|row| {
                let variables: HashMap<String, String> =
                    serde_json::from_str(row.get("variables")).unwrap_or_default();
                let lock_expire_at: Option<String> = row.get("lock_expire_at");
                ExternalTask {
                    task_id: row.get("id"),
                    token_id: row.get("token_id"),
                    process_instance_id: row.get("process_instance_id"),
                    task_type: row.get("task_type"),
                    retries: row.get::<_, i32>("retries"),
                    state: str_to_external_task_state(row.get("state")),
                    lock_owner: row.get("worker_id"),
                    lock_expire_at,
                    error_message: row.get("error_message"),
                    variables,
                    created_at: row.get("created_at"),
                    updated_at: row.get("updated_at"),
                }
            })
            .collect();

        Ok(tasks)
    }

    async fn complete(
        &self,
        task_id: &str,
        worker_id: &str,
        variables: HashMap<String, String>,
    ) -> Result<(), bpm_engine_storage::ExternalTaskError> {
        use bpm_engine_storage::ExternalTaskError;

        let mut client = self
            .pool
            .get()
            .await
            .map_err(|e| ExternalTaskError::Internal(e.to_string()))?;
        let tx = client
            .transaction()
            .await
            .map_err(|e| ExternalTaskError::Internal(e.to_string()))?;

        // Lock the row and classify failures with the same semantics as the
        // memory adapter: TaskNotFound / TaskNotLocked / lease conflict /
        // LockExpired (a worker whose lease expired is rejected even if the
        // lock has not been reclaimed yet).
        let row = tx
            .query_opt(
                r#"
                SELECT variables, state, worker_id, lock_expire_at
                FROM external_task
                WHERE id = $1
                FOR UPDATE
                "#,
                &[&task_id],
            )
            .await
            .map_err(|e| ExternalTaskError::Internal(e.to_string()))?;

        let Some(row) = row else {
            return Err(ExternalTaskError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        };

        let state: String = row.get("state");
        if state != "Locked" {
            return Err(ExternalTaskError::TaskNotLocked {
                task_id: task_id.to_string(),
            });
        }
        let owner: Option<String> = row.get("worker_id");
        if owner.as_deref() != Some(worker_id) {
            return Err(bpm_engine_storage::InvariantViolation::new(
                bpm_engine_storage::InvariantViolationKind::ExternalTaskLeaseConflict,
                format!(
                    "task_id={} expected_owner={:?} actual_worker={}",
                    task_id,
                    owner.as_deref(),
                    worker_id
                ),
            )
            .into());
        }
        // Lock expiry guard (same rule as the memory adapter):
        // lock_expire_at is TEXT unix-epoch seconds; expire <= now rejects.
        let now = unix_epoch_secs();
        if let Some(exp) = row
            .get::<_, Option<String>>("lock_expire_at")
            .and_then(|v| v.parse::<u64>().ok())
        {
            if exp <= now {
                return Err(ExternalTaskError::LockExpired {
                    task_id: task_id.to_string(),
                });
            }
        }

        // Merge variables
        let existing_variables: HashMap<String, String> = row
            .get::<_, Option<String>>("variables")
            .map(|v| serde_json::from_str(&v).unwrap_or_default())
            .unwrap_or_default();
        let mut merged = existing_variables;
        merged.extend(variables);

        let result = tx
            .execute(
                r#"
                UPDATE external_task
                SET state = 'Completed',
                    variables = $2,
                    updated_at = $3,
                    version = version + 1
                WHERE id = $1 AND worker_id = $4 AND state = 'Locked'
                "#,
                &[
                    &task_id,
                    &serde_json::to_string(&merged)
                        .map_err(|e| ExternalTaskError::Internal(e.to_string()))?,
                    &now.to_string(),
                    &worker_id,
                ],
            )
            .await
            .map_err(|e| ExternalTaskError::Internal(e.to_string()))?;

        if result == 0 {
            // The row was locked FOR UPDATE and verified above; a miss here
            // means a concurrent transition — classify as not locked.
            return Err(ExternalTaskError::TaskNotLocked {
                task_id: task_id.to_string(),
            });
        }

        tx.commit()
            .await
            .map_err(|e| ExternalTaskError::Internal(e.to_string()))?;

        Ok(())
    }

    async fn fail(
        &self,
        task_id: &str,
        worker_id: &str,
        error: String,
        retry_after: Option<Duration>,
        permanent: bool,
    ) -> Result<(), bpm_engine_storage::ExternalTaskError> {
        let client = self
            .pool
            .get()
            .await
            .map_err(|e| bpm_engine_storage::ExternalTaskError::Internal(e.to_string()))?;

        if permanent {
            // E3-2 structured failure: skip the retry loop entirely — no
            // decrement, no requeue. The task moves straight to the terminal
            // Failed state (same downstream path as retry exhaustion).
            let now = unix_epoch_secs().to_string();
            let result = client
                .execute(
                    r#"
                    UPDATE external_task
                    SET state = 'Failed',
                        error_message = $3,
                        lock_expire_at = NULL,
                        worker_id = NULL,
                        updated_at = $4,
                        version = version + 1
                    WHERE id = $1 AND worker_id = $2 AND state = 'Locked'
                    "#,
                    &[&task_id, &worker_id, &error, &now],
                )
                .await
                .map_err(|e| bpm_engine_storage::ExternalTaskError::Internal(e.to_string()))?;

            if result == 0 {
                return Err(bpm_engine_storage::ExternalTaskError::TaskNotFound {
                    task_id: task_id.to_string(),
                });
            }
            return Ok(());
        }

        // Get current retries
        let row = client
            .query_opt(
                r#"
                SELECT retries FROM external_task
                WHERE id = $1 AND worker_id = $2 AND state = 'Locked'
                "#,
                &[&task_id, &worker_id],
            )
            .await
            .map_err(|e| bpm_engine_storage::ExternalTaskError::Internal(e.to_string()))?;

        let retries: i32 = row.as_ref().map(|r| r.get("retries")).unwrap_or(0);

        if retries > 0 {
            let lock_expire_epoch: Option<u64> =
                retry_after.map(|d| unix_epoch_secs() + d.as_secs());
            let now = unix_epoch_secs().to_string();

            let result = if let Some(epoch) = lock_expire_epoch {
                client
                    .execute(
                        r#"
                        UPDATE external_task
                        SET state = 'Ready',
                            retries = retries - 1,
                            lock_expire_at = $4,
                            worker_id = NULL,
                            error_message = $3,
                            updated_at = $5,
                            version = version + 1
                        WHERE id = $1 AND worker_id = $2 AND state = 'Locked'
                        "#,
                        &[&task_id, &worker_id, &error, &epoch.to_string(), &now],
                    )
                    .await
                    .map_err(|e| bpm_engine_storage::ExternalTaskError::Internal(e.to_string()))?
            } else {
                client
                    .execute(
                        r#"
                        UPDATE external_task
                        SET state = 'Ready',
                            retries = retries - 1,
                            lock_expire_at = NULL,
                            worker_id = NULL,
                            error_message = $3,
                            updated_at = $4,
                            version = version + 1
                        WHERE id = $1 AND worker_id = $2 AND state = 'Locked'
                        "#,
                        &[&task_id, &worker_id, &error, &now],
                    )
                    .await
                    .map_err(|e| bpm_engine_storage::ExternalTaskError::Internal(e.to_string()))?
            };

            if result == 0 {
                return Err(bpm_engine_storage::ExternalTaskError::TaskNotFound {
                    task_id: task_id.to_string(),
                });
            }
        } else {
            // No more retries, mark as failed. Clear the lease columns
            // like the permanent-failure path and the memory adapter: a
            // terminal row must not carry a stale owner/expiry (the
            // worker no longer holds the lease and will never re-lock).
            let now = unix_epoch_secs().to_string();
            let result = client
                .execute(
                    r#"
                    UPDATE external_task
                    SET state = 'Failed',
                        error_message = $3,
                        lock_expire_at = NULL,
                        worker_id = NULL,
                        updated_at = $4,
                        version = version + 1
                    WHERE id = $1 AND worker_id = $2 AND state = 'Locked'
                    "#,
                    &[&task_id, &worker_id, &error, &now],
                )
                .await
                .map_err(|e| bpm_engine_storage::ExternalTaskError::Internal(e.to_string()))?;

            if result == 0 {
                return Err(bpm_engine_storage::ExternalTaskError::TaskNotFound {
                    task_id: task_id.to_string(),
                });
            }
        }

        Ok(())
    }

    async fn reclaim_expired_locks(&self) -> anyhow::Result<usize> {
        let client = self.pool.get().await?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = client
            .execute(
                r#"
                UPDATE external_task
                SET state = 'Ready', lock_expire_at = NULL, worker_id = NULL
                WHERE state = 'Locked' AND lock_expire_at < $1
                "#,
                &[&now.to_string()],
            )
            .await?;

        Ok(result as usize)
    }

    async fn get(&self, task_id: &str) -> anyhow::Result<Option<ExternalTask>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                r#"
                SELECT id, token_id, process_instance_id, task_type, retries,
                       timeout_secs, variables, state, worker_id, error_message,
                       lock_expire_at, created_at, updated_at
                FROM external_task
                WHERE id = $1
                "#,
                &[&task_id],
            )
            .await?;

        match row {
            Some(row) => {
                let variables: HashMap<String, String> =
                    serde_json::from_str(row.get("variables")).unwrap_or_default();
                let lock_expire_at: Option<String> = row.get("lock_expire_at");
                Ok(Some(ExternalTask {
                    task_id: row.get("id"),
                    token_id: row.get("token_id"),
                    process_instance_id: row.get("process_instance_id"),
                    task_type: row.get("task_type"),
                    retries: row.get::<_, i32>("retries"),
                    state: str_to_external_task_state(row.get("state")),
                    lock_owner: row.get("worker_id"),
                    lock_expire_at,
                    error_message: row.get("error_message"),
                    variables,
                    created_at: row.get("created_at"),
                    updated_at: row.get("updated_at"),
                }))
            }
            None => Ok(None),
        }
    }

    async fn extend_lock(
        &self,
        task_id: &str,
        worker_id: &str,
        extension: Duration,
    ) -> anyhow::Result<bool> {
        let client = self.pool.get().await?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let new_expire = now + extension.as_secs();
        let rows = client
            .execute(
                r#"
                UPDATE external_task
                SET lock_expire_at = $3, updated_at = $4
                WHERE id = $1 AND state = 'Locked' AND worker_id = $2
                "#,
                &[
                    &task_id,
                    &worker_id,
                    &new_expire.to_string(),
                    &now.to_string(),
                ],
            )
            .await?;
        Ok(rows > 0)
    }
}
