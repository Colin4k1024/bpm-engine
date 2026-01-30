//! BPM Worker SDK: pull-based client and worker runtime for external tasks.
//!
//! Use [EngineClient] to talk to the Engine REST API, or [Worker] to run a poll loop
//! with [TaskHandler] implementations.

pub mod client;
pub mod config;
pub mod handler;
pub mod types;
pub mod worker;

pub use client::EngineClient;
pub use config::WorkerConfig;
pub use handler::{TaskContext, TaskHandler};
pub use types::{ExternalTask, TaskResult};
pub use worker::Worker;
