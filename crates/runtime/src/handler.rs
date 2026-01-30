//! EventHandler trait and EngineContext (design: handler.md §5, §6).
//! Context holds async storage store references.

use async_trait::async_trait;
use bpm_core::EngineEvent;
use bpm_storage::{
    CompensationRecordRepo, ExternalTaskStore, OutboxRepo, ParallelJoinRepo,
    ProcessDefinitionStore, ProcessInstanceStore, TimerStore, TokenStore,
};
use std::sync::Arc;

/// Design: handler.md §5 — handle(event, ctx) -> Vec<EngineEvent>. Async for storage access.
#[async_trait]
pub trait EventHandler: Send + Sync {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent>;
}

/// Design: handler.md §6 — execution environment for handlers (stores from storage).
#[derive(Default)]
pub struct EngineContext {
    pub process_store: Option<Arc<dyn ProcessInstanceStore>>,
    pub token_store: Option<Arc<dyn TokenStore>>,
    pub process_def_store: Option<Arc<dyn ProcessDefinitionStore>>,
    pub parallel_join_repo: Option<Arc<dyn ParallelJoinRepo>>,
    pub timer_store: Option<Arc<dyn TimerStore>>,
    pub compensation_repo: Option<Arc<dyn CompensationRecordRepo>>,
    pub outbox_repo: Option<Arc<dyn OutboxRepo>>,
    pub external_task_store: Option<Arc<dyn ExternalTaskStore>>,
    pub tenant_id: Option<String>,
}
