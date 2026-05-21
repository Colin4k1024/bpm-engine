//! EventHandler trait and EngineContext — the dependency injection boundary for the engine.

use async_trait::async_trait;
use bpm_engine_core::EngineEvent;
use bpm_engine_storage::{
    CompensationRecordRepo, ExternalTaskStore, HistoryRepo, OutboxRepo, ParallelJoinRepo,
    ProcessDefinitionStore, ProcessInstanceStore, TimerStore, TokenStore,
};
use std::sync::Arc;

/// Trait implemented by each engine handler (token arrival, process start, etc.).
///
/// Handlers are deterministic and side-effect free beyond storage writes through `ctx`.
/// They receive an event and return zero or more follow-up events for the pump to process.
#[async_trait]
pub trait EventHandler: Send + Sync {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent>;
}

/// Execution context providing access to all storage backends.
///
/// Constructed via [`EngineContext::builder`]. Three stores are required (process instances,
/// tokens, process definitions); all others are optional and enable specific BPMN features
/// (timers, external tasks, compensation, history, parallel joins, outbox).
pub struct EngineContext {
    pub process_store: Arc<dyn ProcessInstanceStore>,
    pub token_store: Arc<dyn TokenStore>,
    pub process_def_store: Arc<dyn ProcessDefinitionStore>,
    pub parallel_join_repo: Option<Arc<dyn ParallelJoinRepo>>,
    pub timer_store: Option<Arc<dyn TimerStore>>,
    pub compensation_repo: Option<Arc<dyn CompensationRecordRepo>>,
    pub outbox_repo: Option<Arc<dyn OutboxRepo>>,
    pub external_task_store: Option<Arc<dyn ExternalTaskStore>>,
    pub history_repo: Option<Arc<dyn HistoryRepo>>,
    pub tenant_id: Option<String>,
}

impl EngineContext {
    /// Create a builder with the three required stores.
    pub fn builder(
        process_store: Arc<dyn ProcessInstanceStore>,
        token_store: Arc<dyn TokenStore>,
        process_def_store: Arc<dyn ProcessDefinitionStore>,
    ) -> EngineContextBuilder {
        EngineContextBuilder {
            process_store,
            token_store,
            process_def_store,
            parallel_join_repo: None,
            timer_store: None,
            compensation_repo: None,
            outbox_repo: None,
            external_task_store: None,
            history_repo: None,
            tenant_id: None,
        }
    }
}

/// Builder for [`EngineContext`] — use method chaining to wire optional stores.
pub struct EngineContextBuilder {
    process_store: Arc<dyn ProcessInstanceStore>,
    token_store: Arc<dyn TokenStore>,
    process_def_store: Arc<dyn ProcessDefinitionStore>,
    parallel_join_repo: Option<Arc<dyn ParallelJoinRepo>>,
    timer_store: Option<Arc<dyn TimerStore>>,
    compensation_repo: Option<Arc<dyn CompensationRecordRepo>>,
    outbox_repo: Option<Arc<dyn OutboxRepo>>,
    external_task_store: Option<Arc<dyn ExternalTaskStore>>,
    history_repo: Option<Arc<dyn HistoryRepo>>,
    tenant_id: Option<String>,
}

impl EngineContextBuilder {
    pub fn parallel_join_repo(mut self, repo: Arc<dyn ParallelJoinRepo>) -> Self {
        self.parallel_join_repo = Some(repo);
        self
    }

    pub fn timer_store(mut self, store: Arc<dyn TimerStore>) -> Self {
        self.timer_store = Some(store);
        self
    }

    pub fn compensation_repo(mut self, repo: Arc<dyn CompensationRecordRepo>) -> Self {
        self.compensation_repo = Some(repo);
        self
    }

    pub fn outbox_repo(mut self, repo: Arc<dyn OutboxRepo>) -> Self {
        self.outbox_repo = Some(repo);
        self
    }

    pub fn external_task_store(mut self, store: Arc<dyn ExternalTaskStore>) -> Self {
        self.external_task_store = Some(store);
        self
    }

    pub fn history_repo(mut self, repo: Arc<dyn HistoryRepo>) -> Self {
        self.history_repo = Some(repo);
        self
    }

    pub fn tenant_id(mut self, id: String) -> Self {
        self.tenant_id = Some(id);
        self
    }

    pub fn build(self) -> EngineContext {
        EngineContext {
            process_store: self.process_store,
            token_store: self.token_store,
            process_def_store: self.process_def_store,
            parallel_join_repo: self.parallel_join_repo,
            timer_store: self.timer_store,
            compensation_repo: self.compensation_repo,
            outbox_repo: self.outbox_repo,
            external_task_store: self.external_task_store,
            history_repo: self.history_repo,
            tenant_id: self.tenant_id,
        }
    }
}
