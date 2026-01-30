use async_trait::async_trait;
use bpm_core::{ProcessDefinition, ProcessInstance};

#[async_trait]
pub trait ProcessInstanceRepo: Send + Sync {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessInstance>>;
    async fn save(&self, instance: &ProcessInstance) -> anyhow::Result<()>;
    async fn list_running(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>>;
}

#[async_trait]
pub trait ProcessDefinitionRepo: Send + Sync {
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessDefinition>>;
}
