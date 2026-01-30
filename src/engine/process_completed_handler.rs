//! ProcessCompletedHandler: ProcessCompleted -> mark instance completed, persist (design: handler.md).

use crate::engine::events::EngineEvent;
use crate::engine::handler::{EngineContext, EventHandler};
use tracing::info;

pub struct ProcessCompletedHandler;

impl EventHandler for ProcessCompletedHandler {
    fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ProcessCompleted(p) = event else {
            return vec![];
        };
        let instance_id = &p.instance_id;
        if let Some(process_repo) = ctx.process_repo.as_ref() {
            if let Some(mut instance) = process_repo.load(instance_id) {
                instance.state = crate::model::InstanceState::Completed;
                process_repo.save(&instance);
            }
        }
        info!(instance_id = %instance_id, "process completed");
        #[cfg(feature = "observability")]
        metrics::counter!("process_completed_total", 1);
        vec![]
    }
}
