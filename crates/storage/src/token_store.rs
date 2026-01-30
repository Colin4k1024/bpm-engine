use async_trait::async_trait;
use bpm_core::Token;

#[async_trait]
pub trait TokenRepo: Send + Sync {
    async fn load_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<Token>>;
    async fn save_tokens(&self, instance_id: &str, tokens: &[Token]) -> anyhow::Result<()>;
    async fn update_token_cas(&self, instance_id: &str, token: &Token) -> anyhow::Result<bool>;
    async fn claim_token(&self, instance_id: &str, token_id: &str, version: u32) -> anyhow::Result<bool>;
}
