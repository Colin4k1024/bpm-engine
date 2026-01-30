use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct TimerRecord {
    pub id: String,
    pub token_id: String,
    pub instance_id: String,
    pub due_at: String,
    pub status: String,
    pub created_at: String,
}

#[async_trait]
pub trait TimerStore: Send + Sync {
    async fn get_by_id(&self, id: &str) -> anyhow::Result<Option<TimerRecord>>;
    async fn mark_fired(&self, id: &str) -> anyhow::Result<()>;
    async fn insert(&self, record: &TimerRecord) -> anyhow::Result<()>;
    async fn list_due(&self, now_iso: &str, limit: u32) -> anyhow::Result<Vec<TimerRecord>>;
}
