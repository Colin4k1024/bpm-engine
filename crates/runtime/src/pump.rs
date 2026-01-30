//! Event pump: run_async(initial_event, ctx) -> loop dispatch to handlers (design: handler.md §9).

use std::collections::VecDeque;

use super::handler::{EngineContext, EventHandler};
use bpm_core::EngineEvent;
use tracing::debug;

pub struct EventPump;

impl EventPump {
    /// Run until queue is empty: pop event, dispatch to all handlers, push new events.
    pub async fn run_async(
        handlers: &[Box<dyn EventHandler>],
        initial: EngineEvent,
        ctx: &mut EngineContext,
    ) {
        let mut queue: VecDeque<EngineEvent> = VecDeque::new();
        queue.push_back(initial);

        while let Some(event) = queue.pop_front() {
            debug!(event = ?event, "event pump dispatch");
            for handler in handlers {
                let new_events = handler.handle(&event, ctx).await;
                queue.extend(new_events);
            }
        }
    }
}
