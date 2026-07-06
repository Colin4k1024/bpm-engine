use async_trait::async_trait;
use bpm_engine_core::{InstanceState, ProcessInstance, Token, TokenMode, TokenStatus};
use bpm_engine_storage::ProcessInstanceStore;
use deadpool_postgres::Pool;
use std::collections::HashMap;

/// PostgreSQL implementation of ProcessInstanceStore using tokio-postgres.
pub struct PostgresProcessStore {
    pool: Pool,
}

impl PostgresProcessStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

fn instance_state_to_str(state: InstanceState) -> &'static str {
    match state {
        InstanceState::Running => "Running",
        InstanceState::Completed => "Completed",
        InstanceState::Terminated => "Terminated",
    }
}

fn str_to_instance_state(s: &str) -> InstanceState {
    match s {
        "Completed" => InstanceState::Completed,
        "Terminated" => InstanceState::Terminated,
        _ => InstanceState::Running,
    }
}

fn token_status_to_str(status: TokenStatus) -> &'static str {
    match status {
        TokenStatus::Created => "Created",
        TokenStatus::Ready => "Ready",
        TokenStatus::Executing => "Executing",
        TokenStatus::Waiting => "Waiting",
        TokenStatus::Suspended => "Suspended",
        TokenStatus::Completed => "Completed",
        TokenStatus::Terminated => "Terminated",
    }
}

fn str_to_token_status(s: &str) -> TokenStatus {
    match s {
        "Created" => TokenStatus::Created,
        "Ready" => TokenStatus::Ready,
        "Executing" => TokenStatus::Executing,
        "Waiting" => TokenStatus::Waiting,
        "Suspended" => TokenStatus::Suspended,
        "Completed" => TokenStatus::Completed,
        "Terminated" => TokenStatus::Terminated,
        _ => TokenStatus::Created,
    }
}

fn token_mode_to_str(mode: TokenMode) -> &'static str {
    match mode {
        TokenMode::Forward => "Forward",
        TokenMode::Compensation => "Compensation",
    }
}

fn str_to_token_mode(s: &str) -> TokenMode {
    match s {
        "Compensation" => TokenMode::Compensation,
        _ => TokenMode::Forward,
    }
}

#[async_trait]
impl ProcessInstanceStore for PostgresProcessStore {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessInstance>> {
        let client = self.pool.get().await?;

        // Load process instance
        let row = client
            .query_opt(
                r#"
                SELECT id, process_def_id, tenant_id, variables, state, version
                FROM process_instance
                WHERE id = $1
                "#,
                &[&id],
            )
            .await?;

        match row {
            Some(row) => {
                // Load tokens for this instance
                let token_rows = client
                    .query(
                        r#"
                        SELECT id, node_id, status, mode, version, attempt, parallel_group_id, updated_at
                        FROM token
                        WHERE instance_id = $1
                        "#,
                        &[&id],
                    )
                    .await?;

                let tokens: Vec<Token> = token_rows
                    .iter()
                    .map(|row| Token {
                        id: row.get("id"),
                        node_id: row.get("node_id"),
                        status: str_to_token_status(row.get("status")),
                        mode: str_to_token_mode(row.get("mode")),
                        version: row.get::<_, i32>("version") as u32,
                        attempt: row.get::<_, i32>("attempt") as u32,
                        parallel_group_id: row.get("parallel_group_id"),
                        updated_at: row.get("updated_at"),
                    })
                    .collect();

                let variables: HashMap<String, String> =
                    serde_json::from_str(row.get("variables")).unwrap_or_default();

                Ok(Some(ProcessInstance {
                    id: row.get("id"),
                    process_def_id: row.get("process_def_id"),
                    tenant_id: row.get("tenant_id"),
                    tokens,
                    variables,
                    state: str_to_instance_state(row.get("state")),
                    version: row.get::<_, i32>("version") as u32,
                    parent_instance_id: row.get("parent_instance_id"),
                    parent_token_id: row.get("parent_token_id"),
                }))
            }
            None => Ok(None),
        }
    }

    async fn save(&self, instance: &ProcessInstance) -> anyhow::Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        // Upsert process_instance
        tx.execute(
            r#"
            INSERT INTO process_instance (id, process_def_id, tenant_id, variables, state, version, parent_instance_id, parent_token_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (id) DO UPDATE SET
                process_def_id = EXCLUDED.process_def_id,
                tenant_id = EXCLUDED.tenant_id,
                variables = EXCLUDED.variables,
                state = EXCLUDED.state,
                version = EXCLUDED.version,
                parent_instance_id = EXCLUDED.parent_instance_id,
                parent_token_id = EXCLUDED.parent_token_id
            "#,
            &[
                &instance.id,
                &instance.process_def_id,
                &instance.tenant_id,
                &serde_json::to_string(&instance.variables)?,
                &instance_state_to_str(instance.state),
                &(instance.version as i32),
                &instance.parent_instance_id,
                &instance.parent_token_id,
            ],
        )
        .await?;

        // Delete existing tokens and insert new ones
        tx.execute("DELETE FROM token WHERE instance_id = $1", &[&instance.id])
            .await?;

        for token in &instance.tokens {
            tx.execute(
                r#"
                INSERT INTO token (id, instance_id, node_id, status, mode, version, attempt, parallel_group_id, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                "#,
                &[
                    &token.id,
                    &instance.id,
                    &token.node_id,
                    &token_status_to_str(token.status),
                    &token_mode_to_str(token.mode),
                    &(token.version as i32),
                    &(token.attempt as i32),
                    &token.parallel_group_id,
                    &token.updated_at,
                ],
            )
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    async fn list_running(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>> {
        let client = self.pool.get().await?;

        let rows = match tenant_id {
            Some(t) => {
                client
                    .query(
                        r#"
                        SELECT id FROM process_instance
                        WHERE state = 'Running' AND (tenant_id = $1 OR (tenant_id IS NULL AND $1 = ''))
                        "#,
                        &[&t],
                    )
                    .await?
            }
            None => {
                client
                    .query(
                        r#"
                        SELECT id FROM process_instance WHERE state = 'Running'
                        "#,
                        &[],
                    )
                    .await?
            }
        };

        Ok(rows.iter().map(|row| row.get("id")).collect())
    }
}
