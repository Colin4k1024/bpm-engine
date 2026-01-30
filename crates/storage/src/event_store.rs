use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct OutboxEvent {
    pub id: String,
    pub event_type: String,
    pub payload: String,
    pub status: String,
    pub tenant_id: Option<String>,
    pub created_at: Option<String>,
}

#[async_trait]
pub trait OutboxRepo: Send + Sync {
    async fn insert_pending(&self, tenant_id: Option<&str>, event_type: &str, payload: &str) -> anyhow::Result<String>;
    async fn list_pending(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<OutboxEvent>>;
    async fn mark_published(&self, id: &str) -> anyhow::Result<()>;
    async fn claim_pending(&self, worker_id: &str, tenant_id: Option<&str>, limit: u32) -> anyhow::Result<Vec<OutboxEvent>>;
    async fn release_claimed(&self, id: &str) -> anyhow::Result<()>;
}
