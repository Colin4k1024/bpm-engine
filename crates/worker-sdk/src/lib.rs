//! BPM Worker SDK: pull-based client and worker runtime for external tasks.
//!
//! Use [EngineClient] to talk to the Engine REST API, or [Worker] to run a poll loop
//! with [TaskHandler] implementations.
//!
//! E3-2 (worker execution context): [TaskContext] carries a
//! [tokio_util::sync::CancellationToken] (see [TaskContext::cancelled]),
//! lease auto-renewal ([TaskContext::spawn_auto_renewal] /
//! [run_with_renewal]), a timeout helper ([with_timeout]), and structured
//! failure categories ([TaskResult::Fail] with
//! `category: Some(RetryCategory::Permanent)` skips the engine's retry
//! loop). [Worker::spawn] returns a [WorkerHandle] with graceful
//! `shutdown`. [IdempotencyGuard] / [derive_key] provide per-process
//! idempotency keys. See `docs/worker-context.md`.

pub mod ai_task;
pub mod client;
pub mod config;
pub mod handler;
pub mod idempotency;
pub mod tools;
pub mod types;
pub mod worker;

pub use client::EngineClient;
pub use config::WorkerConfig;
pub use handler::{run_with_renewal, LeaseRenewalGuard, TaskContext, TaskHandler};
pub use idempotency::{derive_key, IdempotencyGuard, DEFAULT_MAX_KEYS};
pub use tools::{
    apply_output_limit, mask_credentials_in, redact_string, redact_variables, CredentialInjector,
    CredentialSource, OutputLimits, RedactionPolicy, Tool, ToolContext, ToolError, ToolExecutor,
    ToolRegistry, ToolSpec, APPROVAL_VARIABLE,
};
pub use types::{with_timeout, ExternalTask, TaskResult, TASK_TIMEOUT_CODE};
pub use worker::{Worker, WorkerBuilder, WorkerHandle};
