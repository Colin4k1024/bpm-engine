//! BPM engine runtime: event loop, handlers, and gateway evaluation.
//!
//! This crate implements the event-driven execution model. The [`pump::EventPump`]
//! drives [`EngineEvent`]s through [`EventHandler`]s until quiescence.

#![warn(missing_docs)]

/// Call activity handler for subprocess invocation.
pub mod call_activity_handler;
/// EL (Expression Language) evaluator for gateway conditions.
pub mod el;
/// BpmEngine entry point and event loop.
pub mod engine;
/// Runtime error types.
pub mod error;
/// Node executor trait and execution context.
pub mod executor;
/// External task completion handler.
pub mod external_task_completed_handler;
/// Gateway evaluation (exclusive, parallel).
pub mod gateway;
/// EventHandler trait and EngineContext.
pub mod handler;
/// History recording handler.
pub mod history_handler;
/// Message intermediate event handler.
pub mod message_handler;
/// Process completion handler.
pub mod process_completed_handler;
/// Process start handler.
pub mod process_start_handler;
/// Event pump: drives events through handlers until quiescence.
pub mod pump;
/// Token scheduler for polling Ready tokens.
pub mod scheduler;
/// Signal intermediate event handler.
pub mod signal_handler;
/// Timer fired event handler.
pub mod timer_fired_handler;
/// Persistent timer scheduler (background poll loop).
pub mod timer_scheduler;
/// Token arrived handler (main node execution logic).
pub mod token_arrived_handler;
/// Token state transition helpers.
pub mod transition;
/// User task completion handler.
pub mod user_task_completed_handler;

pub use call_activity_handler::*;
pub use el::*;
pub use engine::*;
pub use error::*;
pub use executor::*;
pub use external_task_completed_handler::*;
pub use gateway::*;
pub use handler::*;
pub use history_handler::*;
pub use message_handler::*;
pub use process_completed_handler::*;
pub use process_start_handler::*;
pub use pump::*;
pub use scheduler::*;
pub use signal_handler::*;
pub use timer_fired_handler::*;
pub use timer_scheduler::*;
pub use token_arrived_handler::*;
pub use transition::*;
pub use user_task_completed_handler::*;
