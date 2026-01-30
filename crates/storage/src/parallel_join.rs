use async_trait::async_trait;

#[async_trait]
pub trait ParallelJoinRepo: Send + Sync {
    async fn ensure_group(&self, group_id: &str, expected: u32) -> anyhow::Result<()>;
    async fn try_join(&self, group_id: &str) -> anyhow::Result<bool>;
}
