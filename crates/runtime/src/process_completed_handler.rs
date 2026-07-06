//! ProcessCompletedHandler: ProcessCompleted -> mark instance completed, persist.

use async_trait::async_trait;
use bpm_engine_core::{EngineEvent, InstanceState};
use tracing::info;

use super::handler::{EngineContext, EventHandler};

/// Handler that marks a process instance as completed when all tokens reach terminal states.
pub struct ProcessCompletedHandler;

#[async_trait]
impl EventHandler for ProcessCompletedHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ProcessCompleted(p) = event else {
            return vec![];
        };
        let instance_id = &p.instance_id;
        let process_store = &ctx.process_store;
        if let Ok(Some(mut instance)) = process_store.load(instance_id).await {
            instance.state = InstanceState::Completed;
            let _ = process_store.save(&instance).await;
        }
        info!(instance_id = %instance_id, "process completed");
        vec![]
    }
}
