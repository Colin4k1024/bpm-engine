//! No-op / log-only EventHandler for testing the event pump (design: Step 2).

use super::events::EngineEvent;
use super::handler::{EngineContext, EventHandler};

/// Handler that only logs events and returns no new events (for event pump smoke test).
pub struct LogEventHandler;

impl EventHandler for LogEventHandler {
    fn handle(&self, event: &EngineEvent, _ctx: &mut EngineContext) -> Vec<EngineEvent> {
        println!("[EventPump] {:?}", event);
        vec![]
    }
}
