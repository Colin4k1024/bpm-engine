//! Token store: persistence for process tokens (design: execution-model.md §2).
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use bpm_engine_core::{Token, TokenStatus, TokenMode};
//! use bpm_engine_storage::TokenStore;
//!
//! # async fn example(repo: Arc<impl TokenStore>) -> anyhow::Result<()> {
//! // Save tokens for a process instance
//! let tokens = vec![Token {
//!     id: "token-1".into(),
//!     node_id: "task-1".into(),
//!     status: TokenStatus::Ready,
//!     mode: TokenMode::Forward,
//!     version: 1,
//!     attempt: 0,
//!     parallel_group_id: None,
//!     updated_at: None,
//! }];
//! repo.save_tokens("instance-1", &tokens).await?;
//!
//! // Load tokens by instance
//! let loaded = repo.load_by_instance("instance-1").await?;
//! assert_eq!(loaded.len(), 1);
//!
//! // Claim token (transitions Ready -> Executing)
//! let claimed = repo.claim_token("instance-1", "token-1", 1).await?;
//! assert!(claimed);
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use bpm_engine_core::Token;

/// TokenStore persists process tokens and provides optimistic concurrency control.
///
/// Each token represents a unit of execution authority at a specific node.
/// Tokens transition through states: Ready → Executing → (Completed|Terminated|Waiting).
///
/// # Idempotency
///
/// [`claim_token`] uses compare-and-swap on version to ensure only one caller
/// can claim a token, even under concurrent requests.
#[async_trait]
pub trait TokenStore: Send + Sync {
    /// Load all tokens for a process instance.
    ///
    /// Returns empty vector if instance does not exist.
    async fn load_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<Token>>;

    /// Persist the complete token set for an instance (replace all existing tokens).
    async fn save_tokens(&self, instance_id: &str, tokens: &[Token]) -> anyhow::Result<()>;

    /// Compare-and-swap token update. Returns `true` if version matched and update succeeded.
    ///
    /// Used for safe concurrent token transitions without data loss.
    async fn update_token_cas(&self, instance_id: &str, token: &Token) -> anyhow::Result<bool>;

    /// Atomically claim a Ready token, transitioning it to Executing.
    ///
    /// Fails if token is not in Ready state or version does not match.
    /// This is the entry point for token execution — only one caller succeeds.
    async fn claim_token(
        &self,
        instance_id: &str,
        token_id: &str,
        version: u32,
    ) -> anyhow::Result<bool>;
}
