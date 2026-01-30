use async_trait::async_trait;
use bpm_engine_core::{Token, TokenStatus};
use bpm_engine_storage::TokenStore;
use std::collections::HashMap;
use std::sync::RwLock;

fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

/// In-memory token storage: stores tokens per instance (inside a shared instance map).
/// Used together with MemoryProcessStore; this implementation keeps tokens in the same
/// map as instances for compatibility with existing engine (load_by_instance / save_tokens).
pub struct MemoryTokenStore {
    /// instance_id -> tokens (mirrors ProcessInstance.tokens)
    tokens_by_instance: RwLock<HashMap<String, Vec<Token>>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self {
            tokens_by_instance: RwLock::new(HashMap::new()),
        }
    }

    /// Update tokens for an instance (called when instance is also updated elsewhere).
    pub fn set_tokens(&self, instance_id: &str, tokens: Vec<Token>) {
        self.tokens_by_instance
            .write()
            .unwrap()
            .insert(instance_id.to_string(), tokens);
    }
}

impl Default for MemoryTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TokenStore for MemoryTokenStore {
    async fn load_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<Token>> {
        Ok(self
            .tokens_by_instance
            .read()
            .unwrap()
            .get(instance_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_tokens(&self, instance_id: &str, tokens: &[Token]) -> anyhow::Result<()> {
        self.tokens_by_instance
            .write()
            .unwrap()
            .insert(instance_id.to_string(), tokens.to_vec());
        Ok(())
    }

    async fn update_token_cas(&self, instance_id: &str, token: &Token) -> anyhow::Result<bool> {
        let mut guard = self.tokens_by_instance.write().unwrap();
        let list = match guard.get_mut(instance_id) {
            Some(l) => l,
            None => return Ok(false),
        };
        let pos = match list.iter().position(|t| t.id == token.id) {
            Some(p) => p,
            None => return Ok(false),
        };
        if list[pos].version != token.version {
            return Ok(false);
        }
        list[pos] = token.clone();
        Ok(true)
    }

    async fn claim_token(
        &self,
        instance_id: &str,
        token_id: &str,
        version: u32,
    ) -> anyhow::Result<bool> {
        let mut guard = self.tokens_by_instance.write().unwrap();
        let list = match guard.get_mut(instance_id) {
            Some(l) => l,
            None => return Ok(false),
        };
        let pos = match list.iter().position(|t| t.id == token_id) {
            Some(p) => p,
            None => return Ok(false),
        };
        let t = &list[pos];
        if t.status != TokenStatus::Ready || t.version != version {
            return Ok(false);
        }
        list[pos].status = TokenStatus::Executing;
        list[pos].version += 1;
        list[pos].updated_at = Some(utc_now());
        Ok(true)
    }
}
