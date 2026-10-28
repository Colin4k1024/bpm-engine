//! BPM Worker SDK: pull-based client and worker runtime for external tasks.
//!
//! Use [EngineClient] to talk to the Engine REST API, or [Worker] to run a poll loop
//! with [TaskHandler] implementations.

pub mod ai_task;
pub mod client;
pub mod config;
pub mod handler;
pub mod tools;
pub mod types;
pub mod worker;

pub use client::EngineClient;
pub use config::WorkerConfig;
pub use handler::{TaskContext, TaskHandler};
pub use tools::{
    apply_output_limit, mask_credentials_in, redact_string, redact_variables, CredentialInjector,
    CredentialSource, OutputLimits, RedactionPolicy, Tool, ToolContext, ToolError, ToolExecutor,
    ToolRegistry, ToolSpec, APPROVAL_VARIABLE,
};
pub use types::{ExternalTask, TaskResult};
pub use worker::Worker;
