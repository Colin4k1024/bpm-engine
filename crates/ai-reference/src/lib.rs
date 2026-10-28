//! AI reference flows for the BPM engine (E3-4, issue #18).
//!
//! This crate is the reference implementation of the five AI workflow shapes
//! the engine's AI stack supports — **检索 (retrieval)**, **推理
//! (inference)**, **审批 (approval)**, **工具 (tool)** and **补偿
//! (compensation)** — plus the swappable **model-provider adapter layer**
//! they run against:
//!
//! - [`ModelProvider`] — one `generate` method; applications bring their own
//!   provider. [`MockModelProvider`] is deterministic and fully offline (no
//!   network, no clock, no randomness) and is what the reference suite and
//!   CI always use. [`UnconfiguredProvider`] is the explicit "no provider"
//!   stub; the OpenAI-compatible HTTP adapter (`OpenAiCompatProvider`) lives
//!   behind the default-off `http` feature so the default build stays
//!   offline and light.
//! - The five flows are stored inline as DSL JSON documents
//!   ([`RETRIEVAL_DSL`] … [`COMPENSATION_DSL`], see [`flows`]) and run
//!   through [`run_flow`] over the engine's REST API with a real
//!   [`bpm_engine_worker_sdk::Worker`].
//! - Model tasks complete with the E3-1 [`AiTaskEnvelope`] riding in the
//!   task variables (model ref, cost from the provider's price table,
//!   execution id), so the `/steps` trace joins model versions and cost per
//!   step (`docs/trace.md`).
//! - Tool tasks go through the E3-3 worker-side security boundary
//!   ([`ToolExecutor`]: allowlist + approval gate + env-only credentials +
//!   output masking).
//! - The compensation flow models the engine's saga shape and ships
//!   [`SagaCoordinator`], the application-side compensation loop over the
//!   engine's [`CompensationRecordRepo`](bpm_engine_storage::CompensationRecordRepo).
//! - The **offline regression evaluation harness** (E6-3, [`eval`]) runs a
//!   fixed case set — the five flows plus deliberate variants and failure
//!   injections — against two definition *fixture versions* (v1 as-is vs a
//!   modified v2) and produces a serde-serializable
//!   [`eval::RegressionReport`] with success-rate,
//!   recovery-correctness, human-intervention, latency and cost metrics.
//!   Statistical output is REPORTED, never asserted: the deterministic
//!   invariants live in ordinary tests (`tests/regression_eval.rs`), the
//!   comparative report in the runnable
//!   `examples/regression_eval.rs` harness — see
//!   `docs/regression-eval.md`.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use bpm_engine_ai_reference::{
//!     run_flow, retrieval_flow, MockModelProvider, ModelProvider,
//! };
//!
//! // Offline, deterministic: the mock provider routes by prompt pattern.
//! let provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::new());
//! let outcome = run_flow("http://127.0.0.1:3000", retrieval_flow(), provider).await?;
//! println!("state: {}, summary: {:?}", outcome.state, outcome.variable("summary"));
//! # Ok(())
//! # }
//! ```
//!
//! The flow must be deployed first (see [`deploy_flow`]); `examples/ai_flow.rs`
//! in the repository root shows the fully self-contained version that spawns
//! the server itself.
//!
//! # Offline by default
//!
//! Nothing in the default feature set performs network I/O to a model
//! provider: the mock provider is pure, and the tests/examples talk HTTP only
//! to a locally spawned engine (`127.0.0.1`), exactly like the engine's own
//! REST integration harnesses. `docs/ai-reference-flows.md` documents how to
//! swap in a real provider and how CI runs the suite.

#![warn(missing_docs)]

pub mod eval;
pub mod flows;
pub mod handler;
pub mod provider;
pub mod runner;
pub mod saga;
pub mod tools;

#[cfg(feature = "http")]
pub mod openai;

pub use eval::{
    eval_flow_options, fixed_case_set, fixture_version_selector, run_regression,
    run_regression_with, CaseDelta, CaseExpectations, CaseRun, Injection, RecoveryVerdict,
    RegressionCase, RegressionReport, Version, VersionComparison, VersionSelector, VersionSummary,
    APPROVAL_V2_DSL, COMPENSATION_V2_DSL, INFERENCE_V2_DSL, INJECTED_PERMANENT_FAILURE,
    INJECTED_TRANSIENT_FAILURE, RETRIEVAL_V2_DSL, TOOL_V2_DSL,
};
pub use flows::{
    approval_flow, compensation_flow, compile_flow, deploy_flow, inference_flow, retrieval_flow,
    tool_flow, FlowSpec, UserTaskDecision, APPROVAL_DSL, COMPENSATION_DSL, INFERENCE_DSL,
    RETRIEVAL_DSL, TOOL_DSL,
};
pub use handler::{task_type, ReferenceHandler};
#[cfg(feature = "http")]
pub use openai::{OpenAiCompatProvider, DEFAULT_OPENAI_BASE_URL, DEFAULT_OPENAI_MODEL};
pub use provider::{
    MockModelProvider, ModelProvider, ModelRequest, ModelResponse, ProviderConfig,
    UnconfiguredProvider, MOCK_APPROVED, MOCK_REJECTED, MOCK_SUMMARY_TEXT,
};
pub use runner::{
    run_flow, run_flow_with, FlowOptions, FlowOutcome, StepSummary, DEFAULT_FLOW_TIMEOUT,
};
pub use saga::{
    CompensationOutcome, SagaCoordinator, SAGA_FORWARD_STEPS, STATUS_COMPLETED, STATUS_FAILED,
    STATUS_PENDING,
};
pub use tools::{
    reference_credential_source, reference_executor, LedgerPostTool, LEDGER_TOOL_NAME,
};

/// Re-export of the engine's envelope entry points used by every model task
/// (one import site for applications building on the reference suite).
pub use bpm_engine_core::{AiTaskEnvelope, TaskCost};
