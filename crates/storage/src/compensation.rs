use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct CompensationRecordRow {
    pub id: String,
    pub instance_id: String,
    pub node_id: String,
    pub handler_ref: String,
    pub order: u32,
    pub status: String,
    pub created_at: String,
}

#[async_trait]
pub trait CompensationRecordRepo: Send + Sync {
    async fn add(&self, record: &CompensationRecordRow) -> anyhow::Result<()>;
    async fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow>;
}
