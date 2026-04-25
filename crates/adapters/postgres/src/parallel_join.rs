use async_trait::async_trait;
use bpm_engine_storage::ParallelJoinRepo;
use deadpool_postgres::Pool;

/// PostgreSQL implementation of ParallelJoinRepo.
pub struct PostgresParallelJoinRepo {
    pool: Pool,
}

impl PostgresParallelJoinRepo {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ParallelJoinRepo for PostgresParallelJoinRepo {
    async fn ensure_group(&self, group_id: &str, expected: u32) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                r#"
                INSERT INTO parallel_join (id, parallel_group_id, expected, joined)
                VALUES ($1, $2, $3, 0)
                ON CONFLICT (id) DO UPDATE SET expected = EXCLUDED.expected
                "#,
                &[
                    &uuid::Uuid::new_v4().to_string(),
                    &group_id,
                    &(expected as i32),
                ],
            )
            .await?;
        Ok(())
    }

    async fn try_join(&self, group_id: &str) -> anyhow::Result<bool> {
        let client = self.pool.get().await?;

        // Atomically increment joined and check if we reached expected
        let row = client
            .query_opt(
                r#"
                UPDATE parallel_join
                SET joined = joined + 1
                WHERE parallel_group_id = $1 AND joined < expected
                RETURNING joined, expected
                "#,
                &[&group_id],
            )
            .await?;

        match row {
            Some(r) => {
                let joined: i32 = r.get("joined");
                let expected: i32 = r.get("expected");
                Ok(joined >= expected)
            }
            None => {
                // Either not found or already complete
                let row = client
                    .query_opt(
                        r#"
                        SELECT joined, expected FROM parallel_join
                        WHERE parallel_group_id = $1
                        "#,
                        &[&group_id],
                    )
                    .await?;
                match row {
                    Some(r) => {
                        let joined: i32 = r.get("joined");
                        let expected: i32 = r.get("expected");
                        Ok(joined >= expected)
                    }
                    None => Ok(true), // No group found, treat as complete
                }
            }
        }
    }
}
