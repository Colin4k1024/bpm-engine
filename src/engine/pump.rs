//! Event pump: run(initial_event, ctx) -> loop dispatch to handlers, queue new events (design: handler.md §9).
//! Whitepaper §11.5: when transaction_scope is set, each event's handlers run inside one transaction.

use std::collections::VecDeque;

use super::events::EngineEvent;
use super::handler::{EngineContext, EventHandler};
use tracing::debug;

/// Design: handler.md §9 — event pump.
pub struct EventPump;

impl EventPump {
    /// Run until queue is empty: pop event, dispatch to all handlers, push new events.
    /// When ctx.transaction_scope is Some, each event is handled inside with_tx (one transaction per event).
    pub fn run(handlers: &[Box<dyn EventHandler>], initial: EngineEvent, ctx: &mut EngineContext) {
        let mut queue: VecDeque<EngineEvent> = VecDeque::new();
        queue.push_back(initial);

        while let Some(event) = queue.pop_front() {
            debug!(event = ?event, "event pump dispatch");
            let run_in_tx = ctx.run_in_tx.take();
            if let Some(mut run_in_tx) = run_in_tx {
                run_in_tx(&event, handlers, ctx, &mut queue);
                ctx.run_in_tx = Some(run_in_tx);
            } else {
                for handler in handlers {
                    let new_events = handler.handle(&event, ctx);
                    queue.extend(new_events);
                }
            }
        }
    }
}
