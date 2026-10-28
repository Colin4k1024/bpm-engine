//! The reference worker handler: one [`TaskHandler`] implementation whose
//! [`handle`] dispatches by `task_type` across all five flows.
//!
//! The [`Worker`](bpm_engine_worker_sdk::Worker) fetches by *registered*
//! task type (the SDK's builder keys handlers by `task_type()`), so
//! [`run_flow`](crate::run_flow) registers one thin
//! [`ReferenceHandler`] per flow task type; every instance shares the same
//! [`HandlerDeps`] (provider, pricing, tool wiring), and the dispatch
//! itself switches on the *fetched* task's type:
//!
//! - **Model tasks** (`ref-summarize`, `ref-classify`, `ref-propose`):
//!   build a [`ModelRequest`](crate::ModelRequest) from the task's plain
//!   input variables, call the provider under [`run_with_renewal`] +
//!   [`with_timeout`] (E3-2 ergonomics), and complete with the E3-1
//!   envelope riding in the variables map — model ref from the price-table
//!   config, cost from [`ProviderConfig::cost_for`], execution id from
//!   [`execution_id`], idempotency key from [`derive_key`]. This is the
//!   `complete_with_envelope` wire format exactly: the SDK's own contract
//!   is "a worker completes exactly as today, with the envelope traveling
//!   in the same variables map" — [`TaskOutput::build`] serializes the
//!   envelope into the reserved `__task_envelope` variable, so the
//!   worker runtime's plain `complete` submits byte-identical bodies (see
//!   the wire-format parity test below).
//! - **Tool tasks** (`ref-act`, `ref-run-tool`): resolve credentials from
//!   the worker environment, execute `ledger_post` through the E3-3
//!   boundary (allowlist + approval gate + masking), record the tool in
//!   the envelope.
//! - **Plain tasks** (`ref-retrieve`, the inference branch confirmations,
//!   the saga steps, and the E6-3 `ref-validate` fixture step): deterministic
//!   variable outputs.
//!
//! Tool errors map to [`TaskResult::Fail`] through the SDK's `From` impl:
//! `tool_not_allowed` / `tool_requires_approval` / `credential_missing`
//! are **permanent** (straight to Failed + DLQ — no retry can fix the
//! worker's configuration), `tool_execution_failed` stays transient.
//!
//! [`handle`]: TaskHandler::handle
//! [`run_with_renewal`]: bpm_engine_worker_sdk::run_with_renewal
//! [`with_timeout`]: bpm_engine_worker_sdk::with_timeout
//! [`TaskOutput::build`]: bpm_engine_worker_sdk::ai_task::TaskOutput::build

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bpm_engine_core::{RetryCategory, ToolRef};
use bpm_engine_worker_sdk::ai_task::{new_envelope, TaskInput, TaskOutput};
use bpm_engine_worker_sdk::tools::{CredentialInjector, ToolContext, ToolError, ToolExecutor};
use bpm_engine_worker_sdk::{
    derive_key, run_with_renewal, with_timeout, ExternalTask, TaskContext, TaskHandler, TaskResult,
};

use crate::provider::{ModelProvider, ModelRequest, ProviderConfig};
use crate::runner::FlowOptions;
use crate::saga::SAGA_FORWARD_STEPS;
use crate::tools::{reference_credential_source, LEDGER_TOOL_NAME};

/// Task types of the five reference flows (the dispatch keys).
pub mod task_type {
    /// Retrieval flow: returns the document list.
    pub const RETRIEVE: &str = "ref-retrieve";
    /// Retrieval flow: model task producing `summary`.
    pub const SUMMARIZE: &str = "ref-summarize";
    /// Inference flow: model task producing `verdict`.
    pub const CLASSIFY: &str = "ref-classify";
    /// Approval flow: model task drafting `proposal`.
    pub const PROPOSE: &str = "ref-propose";
    /// Approval flow: side-effect tool task (behind the approval gate).
    pub const ACT: &str = "ref-act";
    /// Inference flow: approved-branch confirmation.
    pub const ACT_APPROVE: &str = "ref-act-approve";
    /// Inference flow: rejected-branch confirmation.
    pub const ACT_REJECT: &str = "ref-act-reject";
    /// Tool flow: side-effect tool task.
    pub const RUN_TOOL: &str = "ref-run-tool";
    /// Compensation flow: reserve step (compensatable).
    pub const SAGA_RESERVE: &str = "ref-saga-reserve";
    /// Compensation flow: charge step (compensatable, decides the outcome).
    pub const SAGA_CHARGE: &str = "ref-saga-charge";
    /// Compensation flow: compensates completed steps in reverse order.
    pub const SAGA_COMPENSATE: &str = "ref-saga-compensate";
    /// Compensation flow: happy-path shipping step.
    pub const SAGA_SHIP: &str = "ref-saga-ship";
    /// Validation step of the E6-3 regression-eval v2 fixture flows
    /// (added definition-change steps): a plain task recording that the
    /// extra check ran.
    pub const VALIDATE: &str = "ref-validate";
}

/// Documents the `retrieve` task returns (deterministic).
pub const REFERENCE_DOCUMENTS: &str = "invoice-2026-Q3.pdf;receipt-2026-Q3.pdf";

/// Shared state behind every [`ReferenceHandler`] instance: the provider,
/// the envelope pricing, the model-call timeout and the reference tool
/// wiring (allowlist + credential source).
pub struct HandlerDeps {
    provider: Arc<dyn ModelProvider>,
    pricing: ProviderConfig,
    model_timeout: Duration,
    executor: ToolExecutor,
    injector: CredentialInjector,
}

impl HandlerDeps {
    /// Wire the shared handler state from the run options.
    pub fn new(provider: Arc<dyn ModelProvider>, options: &FlowOptions) -> Self {
        Self {
            provider,
            pricing: options.pricing.clone(),
            model_timeout: options.model_timeout,
            executor: crate::tools::reference_executor(),
            injector: CredentialInjector::new()
                .source(reference_credential_source(&options.tool_env_var)),
        }
    }
}

/// One thin reference handler. `declared` is the task type it is REGISTERED
/// under (what the worker fetches); the actual dispatch happens on the
/// fetched task's type, so every registration behaves identically.
pub struct ReferenceHandler {
    declared: String,
    deps: Arc<HandlerDeps>,
}

impl ReferenceHandler {
    /// A handler registered under `declared`, sharing `deps`.
    pub fn new(declared: impl Into<String>, deps: Arc<HandlerDeps>) -> Self {
        Self {
            declared: declared.into(),
            deps,
        }
    }

    /// Model task: prompt from `input_var`, response into `output_var`,
    /// envelope (model, cost, execution id, idempotency key) in variables.
    async fn handle_model(
        &self,
        task: &ExternalTask,
        input_var: &str,
        output_var: &str,
        prompt_kind: &str,
    ) -> TaskResult {
        let input = TaskInput::from_task_variables(&task.variables);
        let payload = input.plain(input_var).unwrap_or_default();
        let prompt = format!("{prompt_kind}: {payload}");

        // Plain variables only — envelopes and ciphertext stay opaque to
        // the provider (E2-3 contract).
        let mut variables = HashMap::new();
        for key in input.variables().keys() {
            if let Some(value) = input.plain(key) {
                variables.insert(key.clone(), value.to_string());
            }
        }
        let request = ModelRequest {
            prompt: prompt.clone(),
            variables,
            max_tokens: None,
        };

        // E3-2 ergonomics: a real model call can outlive a lease tick, so
        // wrap it in the timeout helper (a hung provider fails transient
        // with the stable `task_timeout:` code).
        let provider = Arc::clone(&self.deps.provider);
        let generate = async move { provider.generate(request).await };
        let response = match with_timeout(self.deps.model_timeout, generate).await {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                return TaskResult::Fail {
                    error: format!("provider_error: {e}"),
                    retry_after: None,
                    // A provider outage may resolve on retry: transient.
                    category: Some(RetryCategory::Transient),
                };
            }
            Err(timeout) => return timeout,
        };

        // E3-1 envelope: model ref + cost from the price table, execution
        // id, deterministic idempotency key over (task, prompt).
        let mut envelope = new_envelope(&task.task_id, 0);
        envelope.model = Some(self.deps.pricing.model.clone());
        envelope.cost = Some(self.deps.pricing.cost_for(&response));
        envelope.idempotency_key = Some(derive_key(&task.task_id, &prompt));

        let output = TaskOutput::new()
            .variable(output_var, response.text)
            .envelope(envelope);
        finish(output)
    }

    /// Tool task: env credentials, E3-3 boundary, masked output, envelope
    /// recording the executed tool.
    async fn handle_tool(&self, task: &ExternalTask, ctx: &TaskContext) -> TaskResult {
        let input = TaskInput::from_task_variables(&task.variables);

        // 1. Credentials: worker process environment only (never task
        //    variables). Missing env var -> permanent credential_missing.
        let credentials = match self.deps.injector.resolve() {
            Ok(credentials) => credentials,
            Err(e) => return tool_failure(e),
        };

        // 2. The E3-3 boundary: allowlist + approval gate (fail-closed on
        //    the `__approval` task input) + dispatch.
        let tool_ctx = ToolContext::new(&task.task_id, ctx.worker_id())
            .approved(input.approval_granted())
            .credentials(credentials);
        let memo = input.plain("memo").unwrap_or("reference-entry");
        let raw = match self
            .deps
            .executor
            .execute(LEDGER_TOOL_NAME, memo, &tool_ctx)
            .await
        {
            Ok(raw) => raw,
            Err(e) => return tool_failure(e),
        };

        // 3. Output through the security pipeline (redaction, credential
        //    masking, size limit) and the envelope recording the tool.
        let tool_ref = ToolRef::from(
            self.deps
                .executor
                .registry()
                .spec(LEDGER_TOOL_NAME)
                .expect("reference tool is registered"),
        );
        let mut envelope = new_envelope(&task.task_id, 0);
        envelope.tools = vec![tool_ref];
        let output = TaskOutput::new()
            .add_tool_output("ledger_result", &raw, &tool_ctx)
            .envelope(envelope);
        finish(output)
    }

    /// Plain task: complete with fixed variables (no envelope — these are
    /// not AI tasks).
    fn handle_plain(&self, variables: Vec<(&str, &str)>) -> TaskResult {
        let mut output = TaskOutput::new();
        for (name, value) in variables {
            output = output.variable(name, value);
        }
        finish(output)
    }

    /// The compensation step: undo every completed forward step in REVERSE
    /// order (the engine's saga semantics — `crates/core/src/saga.rs`).
    fn handle_compensate(&self) -> TaskResult {
        let log = SAGA_FORWARD_STEPS
            .iter()
            .rev()
            .map(|step| format!("undo-{step}"))
            .collect::<Vec<_>>()
            .join(";");
        self.handle_plain(vec![
            ("compensation_log", log.as_str()),
            ("compensated", "true"),
        ])
    }
}

/// Map a tool error to the E3-2-aware task failure (code-led message,
/// permanent category for policy-boundary errors).
fn tool_failure(e: ToolError) -> TaskResult {
    e.into()
}

/// Finish a model/tool task output: validate the envelope client-side
/// (before anything reaches the wire) and submit via `Complete`.
fn finish(output: TaskOutput) -> TaskResult {
    match output.build() {
        Ok(variables) => TaskResult::Complete { variables },
        Err(e) => TaskResult::Fail {
            error: format!("{}: {}", e.code(), e),
            retry_after: None,
            category: None,
        },
    }
}

#[async_trait]
impl TaskHandler for ReferenceHandler {
    fn task_type(&self) -> &str {
        &self.declared
    }

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
        // E3-2: the whole dispatch runs under automatic lease renewal, so
        // slow providers never lose the lease (instant mock calls finish
        // before the first renewal tick fires).
        run_with_renewal(ctx, |ctx| async move {
            match task.task_type.as_str() {
                task_type::SUMMARIZE => {
                    self.handle_model(&task, "documents", "summary", "summarize")
                        .await
                }
                task_type::CLASSIFY => {
                    self.handle_model(&task, "subject", "verdict", "classify")
                        .await
                }
                task_type::PROPOSE => {
                    self.handle_model(&task, "subject", "proposal", "propose approval for")
                        .await
                }
                task_type::ACT | task_type::RUN_TOOL => self.handle_tool(&task, &ctx).await,
                task_type::RETRIEVE => self.handle_plain(vec![
                    ("documents", REFERENCE_DOCUMENTS),
                    ("retrieved", "true"),
                ]),
                task_type::ACT_APPROVE => self.handle_plain(vec![("action", "approved")]),
                task_type::ACT_REJECT => self.handle_plain(vec![("action", "rejected")]),
                task_type::SAGA_RESERVE => self.handle_plain(vec![("reserved", "true")]),
                task_type::SAGA_CHARGE => {
                    // Business failure: route the saga to its compensation
                    // path (the gateway reads saga_outcome).
                    let input = TaskInput::from_task_variables(&task.variables);
                    let outcome = if input.plain("saga_fail") == Some("true") {
                        "failed"
                    } else {
                        "ok"
                    };
                    self.handle_plain(vec![("charged", "true"), ("saga_outcome", outcome)])
                }
                task_type::SAGA_COMPENSATE => self.handle_compensate(),
                task_type::SAGA_SHIP => self.handle_plain(vec![("shipped", "true")]),
                // E6-3 fixture step: the v2 definition variants insert this
                // plain validation task (no model, no tool, no envelope).
                task_type::VALIDATE => self.handle_plain(vec![("validated", "true")]),
                unknown => TaskResult::Fail {
                    error: format!("unknown_task_type: reference worker cannot handle '{unknown}'"),
                    retry_after: None,
                    // An unknown type is a deployment mismatch: permanent.
                    category: Some(RetryCategory::Permanent),
                },
            }
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_worker_sdk::ai_task::execution_id;

    /// PARITY: the variables `TaskOutput::build` produces (what the worker
    /// runtime submits via plain `complete`) must be byte-identical to what
    /// `EngineClient::complete_with_envelope` would send for the same
    /// envelope — the reference handler's completion path IS the
    /// envelope-aware wire format.
    #[test]
    fn envelope_variables_match_complete_with_envelope_wire_format() {
        let envelope = new_envelope("task-1", 0);
        // What the reference handler submits (envelope rides in variables).
        let from_output = TaskOutput::new()
            .variable("verdict", "approved")
            .envelope(envelope.clone())
            .build()
            .expect("valid envelope");
        // What complete_with_envelope submits: it inserts the serialized
        // envelope at the reserved key of the variables map and calls the
        // same `complete` endpoint (see crates/worker-sdk/src/ai_task.rs).
        let manual: HashMap<String, String> = HashMap::from([
            ("verdict".to_string(), "approved".to_string()),
            (
                bpm_engine_core::AI_TASK_ENVELOPE_KEY.to_string(),
                envelope.to_envelope_value(),
            ),
        ]);
        assert_eq!(from_output, manual);
    }

    fn deps(options: &FlowOptions) -> Arc<HandlerDeps> {
        Arc::new(HandlerDeps::new(
            Arc::new(crate::provider::MockModelProvider::new()),
            options,
        ))
    }

    fn external_task(task_type: &str, variables: &[(&str, &str)]) -> ExternalTask {
        ExternalTask {
            task_id: format!("task-{task_type}"),
            task_type: task_type.to_string(),
            variables: variables
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            lock_expire_at: None,
            retries: 3,
            #[cfg(debug_assertions)]
            token_id: None,
            #[cfg(debug_assertions)]
            process_instance_id: None,
        }
    }

    /// A dead-endpoint client: the handler never contacts the engine inside
    /// these unit tests (renewal warnings against it are harmless).
    fn dead_ctx(task_id: &str) -> TaskContext {
        TaskContext::new(
            "w-unit".to_string(),
            task_id.to_string(),
            bpm_engine_worker_sdk::EngineClient::new("http://127.0.0.1:1"),
        )
    }

    #[tokio::test]
    async fn summarize_task_produces_summary_and_envelope() {
        let handler = ReferenceHandler::new(task_type::SUMMARIZE, deps(&FlowOptions::default()));
        let task = external_task(
            task_type::SUMMARIZE,
            &[("documents", "doc-a;doc-b"), ("topic", "Q3")],
        );
        let result = handler.handle(task, dead_ctx("task-x")).await;
        let TaskResult::Complete { variables } = result else {
            panic!("summarize must complete, got {result:?}");
        };
        assert_eq!(
            variables.get("summary").map(String::as_str),
            Some(crate::provider::MOCK_SUMMARY_TEXT)
        );
        let envelope = TaskInput::from_task_variables(&variables)
            .envelope()
            .expect("envelope present")
            .expect("valid");
        assert_eq!(envelope.execution_id, execution_id("task-ref-summarize", 0));
        assert_eq!(
            envelope.model.as_ref().map(|m| m.name.as_str()),
            Some(crate::provider::MOCK_MODEL_NAME)
        );
        // (48 * 250 + 12 * 500) / 1000 = 18 micro-USD.
        assert_eq!(envelope.cost.as_ref().unwrap().amount_micros, Some(18));
        assert!(envelope.idempotency_key.is_some());
    }

    #[tokio::test]
    async fn classify_task_routes_by_risk_variable() {
        let handler = ReferenceHandler::new(task_type::CLASSIFY, deps(&FlowOptions::default()));
        for (risk, expected) in [("low", "approved"), ("high", "rejected")] {
            let task = external_task(task_type::CLASSIFY, &[("risk", risk), ("subject", "req")]);
            let result = handler.handle(task, dead_ctx("task-x")).await;
            let TaskResult::Complete { variables } = result else {
                panic!("classify must complete, got {result:?}")
            };
            assert_eq!(variables.get("verdict").map(String::as_str), Some(expected));
        }
    }

    #[tokio::test]
    async fn tool_task_without_approval_fails_permanently_with_gate_code() {
        // The credential is PRESENT so the asserted failure is the approval
        // gate, not a missing credential. Unique var: std::env is global
        // and tests run in parallel.
        std::env::set_var("BPM_TEST_AIREF_UNIT_LEDGER2", "sk-unit");
        let options = FlowOptions::default().tool_env_var("BPM_TEST_AIREF_UNIT_LEDGER2");
        let handler = ReferenceHandler::new(task_type::RUN_TOOL, deps(&options));
        let task = external_task(task_type::RUN_TOOL, &[("memo", "entry-1")]);
        let result = handler.handle(task, dead_ctx("task-x")).await;
        match result {
            TaskResult::Fail {
                error, category, ..
            } => {
                assert!(error.starts_with("tool_requires_approval"), "{error}");
                assert_eq!(category, Some(RetryCategory::Permanent));
            }
            TaskResult::Complete { .. } => panic!("unapproved side-effect tool must fail"),
        }
        std::env::remove_var("BPM_TEST_AIREF_UNIT_LEDGER2");
    }

    #[tokio::test]
    async fn tool_task_with_missing_credential_fails_permanently() {
        std::env::remove_var("BPM_TEST_AIREF_UNIT_LEDGER4");
        let options = FlowOptions::default().tool_env_var("BPM_TEST_AIREF_UNIT_LEDGER4");
        let handler = ReferenceHandler::new(task_type::RUN_TOOL, deps(&options));
        let task = external_task(
            task_type::RUN_TOOL,
            &[("memo", "entry-3"), ("__approval", "true")],
        );
        let result = handler.handle(task, dead_ctx("task-x")).await;
        match result {
            TaskResult::Fail {
                error, category, ..
            } => {
                assert!(error.starts_with("credential_missing"), "{error}");
                assert!(error.contains("BPM_TEST_AIREF_UNIT_LEDGER4"));
                assert_eq!(category, Some(RetryCategory::Permanent));
            }
            TaskResult::Complete { .. } => panic!("missing credential must fail"),
        }
    }

    #[tokio::test]
    async fn tool_task_with_approval_masks_credential() {
        std::env::set_var("BPM_TEST_AIREF_UNIT_LEDGER3", "sk-mask-me");
        let options = FlowOptions::default().tool_env_var("BPM_TEST_AIREF_UNIT_LEDGER3");
        let handler = ReferenceHandler::new(task_type::RUN_TOOL, deps(&options));
        let task = external_task(
            task_type::RUN_TOOL,
            &[("memo", "entry-2"), ("__approval", "true")],
        );
        let result = handler.handle(task, dead_ctx("task-x")).await;
        let TaskResult::Complete { variables } = result else {
            panic!("approved tool must complete, got {result:?}")
        };
        let ledger_result = variables.get("ledger_result").expect("output present");
        // The credential is masked by the boundary pipeline.
        assert_eq!(
            ledger_result.as_str(),
            "ledger: posted 'entry-2' with key=***"
        );
        // The envelope records the executed tool.
        let envelope = TaskInput::from_task_variables(&variables)
            .envelope()
            .expect("envelope present")
            .expect("valid");
        assert_eq!(envelope.tools.len(), 1);
        assert_eq!(envelope.tools[0].name, LEDGER_TOOL_NAME);
        std::env::remove_var("BPM_TEST_AIREF_UNIT_LEDGER3");
    }

    #[tokio::test]
    async fn compensate_task_undoes_forward_steps_in_reverse_order() {
        let handler =
            ReferenceHandler::new(task_type::SAGA_COMPENSATE, deps(&FlowOptions::default()));
        let task = external_task(task_type::SAGA_COMPENSATE, &[]);
        let result = handler.handle(task, dead_ctx("task-x")).await;
        let TaskResult::Complete { variables } = result else {
            panic!("compensate must complete, got {result:?}")
        };
        assert_eq!(
            variables.get("compensation_log").map(String::as_str),
            Some("undo-charge;undo-reserve")
        );
    }

    #[tokio::test]
    async fn plain_tasks_emit_their_variables() {
        let handler = ReferenceHandler::new(task_type::RETRIEVE, deps(&FlowOptions::default()));
        let task = external_task(task_type::RETRIEVE, &[("topic", "Q3")]);
        let result = handler.handle(task, dead_ctx("task-x")).await;
        let TaskResult::Complete { variables } = result else {
            panic!("retrieve must complete, got {result:?}")
        };
        assert_eq!(
            variables.get("documents").map(String::as_str),
            Some(REFERENCE_DOCUMENTS)
        );
        // Plain (non-AI) tasks carry no envelope.
        assert!(TaskInput::from_task_variables(&variables)
            .envelope()
            .is_none());
    }

    #[tokio::test]
    async fn validate_task_emits_its_marker_variable() {
        // The E6-3 v2 fixture step: plain, deterministic, no envelope.
        let handler = ReferenceHandler::new(task_type::VALIDATE, deps(&FlowOptions::default()));
        let task = external_task(task_type::VALIDATE, &[]);
        let result = handler.handle(task, dead_ctx("task-x")).await;
        let TaskResult::Complete { variables } = result else {
            panic!("validate must complete, got {result:?}")
        };
        assert_eq!(variables.get("validated").map(String::as_str), Some("true"));
        assert!(TaskInput::from_task_variables(&variables)
            .envelope()
            .is_none());
    }

    #[tokio::test]
    async fn unknown_task_type_fails_permanently() {
        let handler = ReferenceHandler::new("ref-unknown", deps(&FlowOptions::default()));
        let task = external_task("ref-unknown", &[]);
        let result = handler.handle(task, dead_ctx("task-x")).await;
        match result {
            TaskResult::Fail {
                error, category, ..
            } => {
                assert!(error.starts_with("unknown_task_type"), "{error}");
                assert_eq!(category, Some(RetryCategory::Permanent));
            }
            TaskResult::Complete { .. } => panic!("unknown type must fail"),
        }
    }
}
