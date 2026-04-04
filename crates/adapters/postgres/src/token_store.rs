use async_trait::async_trait;
use bpm_engine_core::{Token, TokenMode, TokenStatus};
use bpm_engine_storage::TokenStore;
use deadpool_postgres::Pool;

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

/// PostgreSQL implementation of TokenStore using tokio-postgres.
pub struct PostgresTokenStore {
    pool: Pool,
}

impl PostgresTokenStore {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TokenStore for PostgresTokenStore {
    async fn load_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<Token>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                r#"
                SELECT id, node_id, status, mode, version, attempt, parallel_group_id, updated_at
                FROM token
                WHERE instance_id = $1
                "#,
                &[&instance_id],
            )
            .await?;

        let tokens = rows
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

        Ok(tokens)
    }

    async fn save_tokens(&self, instance_id: &str, tokens: &[Token]) -> anyhow::Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        // Delete existing tokens for this instance
        tx.execute("DELETE FROM token WHERE instance_id = $1", &[&instance_id])
            .await?;

        // Insert all tokens
        for token in tokens {
            tx.execute(
                r#"
                INSERT INTO token (id, instance_id, node_id, status, mode, version, attempt, parallel_group_id, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                "#,
                &[
                    &token.id,
                    &instance_id,
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

    async fn update_token_cas(&self, instance_id: &str, token: &Token) -> anyhow::Result<bool> {
        let client = self.pool.get().await?;
        let result = client
            .execute(
                r#"
                UPDATE token
                SET node_id = $3,
                    status = $4,
                    mode = $5,
                    version = $6,
                    attempt = $7,
                    parallel_group_id = $8,
                    updated_at = $9
                WHERE id = $1
                  AND instance_id = $2
                  AND version = $10
                "#,
                &[
                    &token.id,
                    &instance_id,
                    &token.node_id,
                    &token_status_to_str(token.status),
                    &token_mode_to_str(token.mode),
                    &(token.version as i32),
                    &(token.attempt as i32),
                    &token.parallel_group_id,
                    &token.updated_at,
                    &((token.version - 1) as i32),
                ],
            )
            .await?;

        Ok(result == 1)
    }

    async fn claim_token(
        &self,
        instance_id: &str,
        token_id: &str,
        version: u32,
    ) -> anyhow::Result<bool> {
        let client = self.pool.get().await?;
        let result = client
            .execute(
                r#"
                UPDATE token
                SET status = 'Executing',
                    version = version + 1,
                    updated_at = NOW()::text
                WHERE id = $1
                  AND instance_id = $2
                  AND status = 'Ready'
                  AND version = $3
                "#,
                &[&token_id, &instance_id, &(version as i32)],
            )
            .await?;

        Ok(result == 1)
    }
}
