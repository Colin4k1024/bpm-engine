use super::handler::{EngineContext, EventHandler};
use super::pump::EventPump;
use bpm_core::EngineEvent;
use bpm_storage::TokenStore;
use std::sync::Arc;

pub struct Engine<S> {
    store: Arc<S>,
}

impl<S> Engine<S>
where
    S: TokenStore + Send + Sync + 'static,
{
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    pub async fn tick(&self) -> anyhow::Result<()> {
        // 1. claim tokens
        // 2. execute
        // 3. persist
        let _ = &self.store;
        Ok(())
    }
}

/// BpmEngine aggregates handlers and runs event pump (design: overview §3).
pub struct BpmEngine {
    pub handlers: Vec<Box<dyn EventHandler>>,
}

impl BpmEngine {
    pub fn new(handlers: Vec<Box<dyn EventHandler>>) -> Self {
        BpmEngine { handlers }
    }

    /// Run event pump with initial event (async; uses storage via ctx).
    pub async fn run_async(&self, initial: EngineEvent, ctx: &mut EngineContext) {
        EventPump::run_async(&self.handlers, initial, ctx).await;
    }
}
