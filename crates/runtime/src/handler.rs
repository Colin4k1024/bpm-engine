//! EventHandler trait and EngineContext (design: handler.md §5, §6).
//! Context holds async storage store references.

use async_trait::async_trait;
use bpm_core::EngineEvent;
use bpm_storage::{
    CompensationRecordRepo, ExternalTaskStore, OutboxRepo, ParallelJoinRepo, ProcessDefinitionRepo,
    ProcessInstanceRepo, TimerRepo, TokenRepo,
};
use std::sync::Arc;

/// Design: handler.md §5 — handle(event, ctx) -> Vec<EngineEvent>. Async for storage access.
#[async_trait]
pub trait EventHandler: Send + Sync {
    async fn handle(
        &self,
        event: &EngineEvent,
        ctx: &mut EngineContext,
    ) -> Vec<EngineEvent>;
}

/// Design: handler.md §6 — execution environment for handlers (repos from storage).
pub struct EngineContext {
    pub process_repo: Option<Arc<dyn ProcessInstanceRepo>>,
    pub token_repo: Option<Arc<dyn TokenRepo>>,
    pub process_def_repo: Option<Arc<dyn ProcessDefinitionRepo>>,
    pub parallel_join_repo: Option<Arc<dyn ParallelJoinRepo>>,
    pub timer_repo: Option<Arc<dyn TimerRepo>>,
    pub compensation_repo: Option<Arc<dyn CompensationRecordRepo>>,
    pub outbox_repo: Option<Arc<dyn OutboxRepo>>,
    pub external_task_repo: Option<Arc<dyn ExternalTaskStore>>,
    pub tenant_id: Option<String>,
}

impl Default for EngineContext {
    fn default() -> Self {
        EngineContext {
            process_repo: None,
            token_repo: None,
            process_def_repo: None,
            parallel_join_repo: None,
            timer_repo: None,
            compensation_repo: None,
            outbox_repo: None,
            external_task_repo: None,
            tenant_id: None,
        }
    }
}
