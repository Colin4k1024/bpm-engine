//! External Task store trait (plan §12): fetch-and-lock, complete, fail, reclaim, create.

use async_trait::async_trait;
use bpm_core::ExternalTask;
use std::collections::HashMap;
use std::time::Duration;

#[async_trait]
pub trait ExternalTaskStore: Send + Sync {
    /// Create a READY external task when token arrives at ExternalTask node.
    async fn create(
        &self,
        token_id: &str,
        process_instance_id: &str,
        task_type: &str,
        retries: i32,
        timeout_secs: u64,
        variables: HashMap<String, String>,
    ) -> anyhow::Result<String>;

    /// Fetch READY tasks matching task_types, lock to worker, return locked tasks.
    async fn fetch_and_lock(
        &self,
        worker_id: &str,
        task_types: &[String],
        max_tasks: usize,
        lock_duration: Duration,
    ) -> anyhow::Result<Vec<ExternalTask>>;

    /// Complete: LOCKED + lock_owner + not expired -> COMPLETED; merge variables.
    async fn complete(
        &self,
        task_id: &str,
        worker_id: &str,
        variables: HashMap<String, String>,
    ) -> anyhow::Result<()>;

    /// Fail: retries -= 1; if retries > 0 -> READY else -> FAILED.
    async fn fail(
        &self,
        task_id: &str,
        worker_id: &str,
        error: String,
        retry_after: Option<Duration>,
    ) -> anyhow::Result<()>;

    /// Reclaim LOCKED tasks whose lock_expire_at < now to READY (plan §9).
    async fn reclaim_expired_locks(&self) -> anyhow::Result<usize>;

    /// Load task by id (for REST layer after complete to get token_id/instance_id for transition).
    async fn get(&self, task_id: &str) -> anyhow::Result<Option<ExternalTask>>;
}
