//! TaskHandler trait and TaskContext (design §1). User implements TaskHandler; runtime passes TaskContext.

use async_trait::async_trait;
use std::time::Duration;

use crate::client::EngineClient;
use crate::types::{ExternalTask, TaskResult};

/// Context passed to the handler. Provides access to task metadata and
/// the ability to extend the lock for long-running tasks.
#[derive(Clone)]
pub struct TaskContext {
    pub(crate) worker_id: String,
    pub(crate) task_id: String,
    pub(crate) client: EngineClient,
}

impl TaskContext {
    pub fn new(worker_id: String, task_id: String, client: EngineClient) -> Self {
        Self {
            worker_id,
            task_id,
            client,
        }
    }

    /// The worker ID that owns this task's lock.
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// The task ID.
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Extend the lock on this task to prevent timeout during long processing.
    ///
    /// Call this periodically for tasks that may exceed the initial lock duration.
    pub async fn extend_lock(&self, extension: Duration) -> Result<(), crate::client::ClientError> {
        self.client
            .extend_lock(&self.task_id, &self.worker_id, extension)
            .await
    }
}

/// User implements this to handle tasks (design §1).
#[async_trait]
pub trait TaskHandler: Send + Sync {
    fn task_type(&self) -> &str;

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult;
}
