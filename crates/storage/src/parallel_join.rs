//! Parallel join coordination: ensures all branches complete before join proceeds.
//!
//! # Example
//!
//! ```ignore
//! let repo = Arc::new(MemoryRepo::new());
//!
//! // Register a parallel group expecting 3 branches
//! repo.ensure_group("fork-1", 3).await?;
//!
//! // First two branches arrive — not enough
//! let joined = repo.try_join("fork-1").await?;
//! assert!(!joined, "not all branches arrived yet");
//!
//! repo.try_join("fork-1").await?;
//! let joined = repo.try_join("fork-1").await?;
//! assert!(joined, "all 3 branches have arrived");
//! ```

use async_trait::async_trait;

/// ParallelJoinRepo coordinates fork/join gates in BPMN processes.
///
/// When a parallel fork creates N tokens, the join node must wait until all N tokens
/// have arrived before firing the continuation token. [`try_join`] atomically
/// increments the counter and returns `true` when the last branch arrives.
#[async_trait]
pub trait ParallelJoinRepo: Send + Sync {
    /// Register a parallel group with the expected number of branches.
    async fn ensure_group(&self, group_id: &str, expected: u32) -> anyhow::Result<()>;

    /// Record one branch arriving at the join. Returns `true` when all branches have arrived.
    async fn try_join(&self, group_id: &str) -> anyhow::Result<bool>;
}
