//! BPM Engine (design: overview §3).
//! TokenScheduler, NodeExecutor, GatewayEvaluator, EventBus / event pump.

pub mod el;
mod events;
mod executor;
mod gateway;
mod handler;
mod log_handler;
mod process_completed_handler;
mod process_start_handler;
mod pump;
mod scheduler;
mod saga;
mod timer_handler;
mod token_arrived_handler;
mod transition;
mod user_task_completed_handler;

pub use events::{payloads, EngineEvent};
pub use gateway::GatewayEvaluator;
pub use handler::{EngineContext, EventHandler};
pub use pump::EventPump;
pub use scheduler::TokenScheduler;
pub use executor::NodeExecutor;
pub use log_handler::LogEventHandler;
pub use process_completed_handler::ProcessCompletedHandler;
pub use process_start_handler::ProcessStartHandler;
pub use token_arrived_handler::TokenArrivedHandler;
pub use saga::{CompensationRecord, CompensationStatus, SagaCoordinator};
pub use timer_handler::{Timer, TimerFiredHandler, TimerType};
pub use user_task_completed_handler::UserTaskCompletedHandler;

/// BpmEngine aggregates scheduler, executor, gateway, event pump (design: overview §3).
pub struct BpmEngine {
    pub handlers: Vec<Box<dyn EventHandler>>,
}

impl BpmEngine {
    pub fn new(handlers: Vec<Box<dyn EventHandler>>) -> Self {
        BpmEngine { handlers }
    }

    /// Run event pump with initial event (design: handler.md §9).
    pub fn run(&self, initial: EngineEvent, ctx: &mut EngineContext) {
        EventPump::run(&self.handlers, initial, ctx);
    }
}
