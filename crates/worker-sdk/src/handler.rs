//! TaskHandler trait and TaskContext (design §1). User implements TaskHandler; runtime passes TaskContext.

use async_trait::async_trait;

use crate::types::{ExternalTask, TaskResult};

/// Context passed to the handler; placeholder for extend_lock etc. (Phase 2).
#[derive(Debug, Clone)]
pub struct TaskContext {
    #[allow(dead_code)]
    pub(crate) worker_id: String,
    #[allow(dead_code)]
    pub(crate) task_id: String,
}

impl TaskContext {
    pub fn new(worker_id: String, task_id: String) -> Self {
        Self { worker_id, task_id }
    }
}

/// User implements this to handle tasks (design §1).
#[async_trait]
pub trait TaskHandler: Send + Sync {
    fn task_type(&self) -> &str;

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult;
}
