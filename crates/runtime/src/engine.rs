//! BpmEngine: event-driven workflow execution engine.
//!
//! # Example
//!
//! ```
//! // BpmEngine is constructed with event handlers and run with an initial EngineEvent.
//! // The engine drives the initial event through all handlers until no further events are produced.
//! //
//! // let engine = BpmEngine::new(vec![...handlers...]);
//! // engine.run_async(initial_event, &mut ctx).await;
//! ```

use super::handler::{EngineContext, EventHandler};
use super::pump::EventPump;
use bpm_engine_core::EngineEvent;

/// BpmEngine aggregates handlers and runs event pump (design: overview §3).
///
/// BpmEngine is the main entry point for event-driven workflow execution.
/// It holds a collection of [`EventHandler`]s that process [`EngineEvent`]s
/// deterministically and transactionally.
pub struct BpmEngine {
    /// Registered event handlers, applied in order for each event.
    pub handlers: Vec<Box<dyn EventHandler>>,
}

impl BpmEngine {
    /// Create a BpmEngine with the given event handlers.
    ///
    /// Handlers are applied in registration order for each incoming event.
    pub fn new(handlers: Vec<Box<dyn EventHandler>>) -> Self {
        BpmEngine { handlers }
    }

    /// Run event pump with initial event (async; uses storage via ctx).
    ///
    /// This is the main async entry point. The event pump will drive
    /// the initial event through all handlers until no further events are produced.
    pub async fn run_async(&self, initial: EngineEvent, ctx: &mut EngineContext) {
        EventPump::run_async(&self.handlers, initial, ctx).await;
    }
}
