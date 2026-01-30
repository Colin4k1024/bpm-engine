//! EventHandler trait and EngineContext (design: handler.md §5, §6).

use crate::engine::events::EngineEvent;

/// Design: handler.md §5 — handle(event, ctx) -> Vec<EngineEvent>.
pub trait EventHandler {
    fn handle(
        &self,
        event: &EngineEvent,
        ctx: &mut EngineContext,
    ) -> Vec<EngineEvent>;
}

/// Design: handler.md §6 — execution environment for handlers (repos, services, clock).
/// Whitepaper §11.5: run_in_tx runs each event's handlers inside one transaction when set.
pub struct EngineContext {
    pub process_repo: Option<Box<dyn crate::persistence::ProcessInstanceRepo>>,
    pub token_repo: Option<Box<dyn crate::persistence::TokenRepo>>,
    pub process_def_repo: Option<Box<dyn crate::persistence::ProcessDefinitionRepo>>,
    pub task_repo: Option<Box<dyn crate::persistence::UserTaskRepo>>,
    /// Whitepaper §11.7: atomic parallel join (optional).
    pub parallel_join_repo: Option<Box<dyn crate::persistence::ParallelJoinRepo>>,
    /// Timer repo for TimerFiredHandler (design: timer.md §7).
    pub timer_repo: Option<Box<dyn crate::persistence::TimerRepo>>,
    /// Compensation record repo for SagaCoordinator (design: saga.md).
    pub compensation_repo: Option<Box<dyn crate::persistence::CompensationRecordRepo>>,
    /// When set, each event's handlers run inside this closure (one transaction per event).
    pub run_in_tx: Option<Box<dyn FnMut(&crate::engine::events::EngineEvent, &[Box<dyn EventHandler>], &mut EngineContext, &mut std::collections::VecDeque<crate::engine::events::EngineEvent>)>>,
}

impl Default for EngineContext {
    fn default() -> Self {
        EngineContext {
            process_repo: None,
            token_repo: None,
            process_def_repo: None,
            task_repo: None,
            parallel_join_repo: None,
            timer_repo: None,
            compensation_repo: None,
            run_in_tx: None,
        }
    }
}
