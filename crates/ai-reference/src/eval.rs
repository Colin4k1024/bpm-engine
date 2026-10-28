//! Offline regression evaluation harness (E6-3, issue #28).
//!
//! **The separation principle — 统计指标与确定性内核测试分开.** This
//! module produces a *REPORT*, not pass/fail:
//!
//! - The **deterministic kernel tests** (exact behavior: flows complete,
//!   routing is right, the DLQ is reached, attempts are counted) live as
//!   ordinary `#[test]`s in `crates/ai-reference/tests/reference_flows.rs`
//!   (per-flow semantics) and `crates/ai-reference/tests/regression_eval.rs`
//!   (a small subset of THIS harness's cases, asserting exact completion
//!   states, exact attempt counts, exact costs per the mock price table and
//!   the report's structural validity). They run in every CI
//!   `cargo test --workspace`.
//! - The **statistical/comparative output** — success-rate deltas, latency
//!   distributions, cost totals per definition version — is produced by the
//!   runnable harness `crates/ai-reference/examples/regression_eval.rs`
//!   (`cargo run -p bpm-engine-ai-reference --example regression_eval`):
//!   it runs the FULL fixed case set against BOTH definition versions on a
//!   self-spawned server and writes `regression-report.json` plus a
//!   markdown summary. **Wall-clock latency and the aggregated rates are
//!   reported only — never asserted.** The deterministic proxies (step
//!   counts, attempt counts, envelope costs) are what tests can pin.
//!
//! # What it evaluates
//!
//! A fixed sample set of [`RegressionCase`]s built from the five reference
//! flows plus deliberate variants (inference low/high risk, approval
//! approved/unapproved, and the three failure-injection recovery cases —
//! see [`Injection`]). Every case runs against **two definition
//! versions**:
//!
//! - **v1** — the flow DSL as-is (the current reference definitions);
//! - **v2** — a modified *fixture variant* of each flow representing a
//!   "definition change": the retrieval flow gains a validation step after
//!   the summary, the inference flow flips the gateway default (a
//!   behavior-preserving refactor), the approval flow inserts a validation
//!   step before the side effect, the tool flow tightens `retries` 3 → 1
//!   (a *behavior-changing* tightening: a single transient failure then
//!   exhausts the budget), and the compensation flow audits after undoing.
//!   The versions are registered under `:2`-suffixed definition ids
//!   (`ai-reference-tool:2`, …), so both coexist in the engine's definition
//!   store and each instance runs against exactly one of them. Honest
//!   scoping: the "versions" are fixture variants inside the harness —
//!   that IS the comparison mechanism; see
//!   [`fixture_version_selector`].
//!
//! The metrics (definitions in `docs/regression-eval.md`):
//!
//! | metric | definition | asserted? |
//! |---|---|---|
//! | `success_rate` | completed cases / total, per version | report only; the per-case `completed` flag is deterministic and testable |
//! | `recovery_correctness` | per injection case: transient → redelivered attempt completes; crash → expired lease redelivers and completes; permanent → DLQ reached with NO retry storm (exactly 1 attempt) | deterministic verdict per case (testable); the aggregate rate is report only |
//! | `human_intervention_rate` | intervention points / decision points (user-task decisions submitted + `__approval` inputs required) | deterministic counts (testable); the rate is report only |
//! | `latency_ms` | wall clock per case | **reported only** — plus the deterministic proxies `steps` and `attempts` |
//! | `cost` | sum of the E6-1 envelope `TaskCost` (amount_micros, tokens) | deterministic given the mock provider's price table — exact values are testable |
//!
//! Everything runs offline: the mock [`ModelProvider`] is deterministic, and
//! the only HTTP is to the locally spawned engine (loopback), exactly like
//! the crate's other integration harnesses.
//!
//! # Failure injection
//!
//! A case may declare [`Injection`] on the flow's **first service task**
//! (the DSL's first `serviceTask` node — the injection target and its task
//! type are derived from the DSL document):
//!
//! - [`Injection::TransientFailOnce`] — the first attempt fails with an
//!   E3-2 *transient* category (`injected_transient_failure: …`); the
//!   engine decrements retries and redelivers; the second attempt runs the
//!   real handler.
//! - [`Injection::PermanentFail`] — every attempt of the target fails with
//!   a *permanent* category (`injected_permanent_failure: …`): straight to
//!   `Failed` + DLQ + `TokenFailed`, no retry storm.
//! - [`Injection::CrashAndRedeliver`] — the first attempt sleeps past the
//!   lease (no renewal — the real handler has not started yet) and then
//!   returns a stale result whose submission the engine rejects, exactly
//!   like a worker that crashed mid-handle: the lease expires, the engine
//!   redelivers, and the second attempt completes. Timings stay SHORT: the
//!   eval profile ([`eval_flow_options`]) uses a 2 s lease (the store's
//!   lease expiry is second-granular — 1 s would be a 0–1000 ms true
//!   lease) and a 200 ms worker poll.
//!
//! # Example
//!
//! ```no_run
//! # async fn example(
//! #     deployer: &dyn bpm_engine_storage::DefinitionDeployer,
//! # ) -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use bpm_engine_ai_reference::eval::{
//!     self, fixed_case_set, fixture_version_selector, run_regression, Version,
//! };
//! use bpm_engine_ai_reference::MockModelProvider;
//!
//! let provider = Arc::new(MockModelProvider::new());
//! let selector = fixture_version_selector();
//! let report = run_regression(
//!     "http://127.0.0.1:3000",
//!     deployer,
//!     provider,
//!     &fixed_case_set(),
//!     &selector,
//! )
//! .await?;
//! println!("{}", report.markdown_summary());
//! # let _ = Version::V2; // the harness always compares v1 vs v2
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use async_trait::async_trait;
use bpm_engine_core::RetryCategory;
use bpm_engine_storage::DefinitionDeployer;
use bpm_engine_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::flows::{deploy_flow, FlowSpec};
use crate::handler::{task_type, HandlerDeps, ReferenceHandler};
use crate::provider::ModelProvider;
use crate::runner::{build_outcome, FlowOptions, FlowOutcome, Rest, StepSummary};

// ---------------------------------------------------------------------------
// Version model
// ---------------------------------------------------------------------------

/// A definition version the regression harness runs a case against.
///
/// The harness always compares both: [`Version::V1`] is the flow DSL as-is,
/// [`Version::V2`] the modified fixture variant selected by the
/// [`VersionSelector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Version {
    /// The flow as-is (the current reference definitions).
    V1,
    /// The modified fixture variant (the candidate definition change).
    V2,
}

impl Version {
    /// The report label of this version (`"v1"` / `"v2"`).
    pub fn label(self) -> &'static str {
        match self {
            Version::V1 => "v1",
            Version::V2 => "v2",
        }
    }

    /// Every version the harness compares, in run order.
    pub fn all() -> [Version; 2] {
        [Version::V1, Version::V2]
    }
}

/// Maps a case to the [`FlowSpec`] of one definition version.
///
/// v1 must return the case's own flow; v2 returns the modified variant
/// (a different DSL document, a `:2`-suffixed definition id and its task
/// types, with the case's start variables and user-task decisions grafted
/// on). The shipped selector for the five reference flows is
/// [`fixture_version_selector`]; unknown flows degrade to the identity
/// mapping (documented — extend the fixture table or pass your own).
pub type VersionSelector = Arc<dyn Fn(&RegressionCase, Version) -> FlowSpec + Send + Sync>;

// ---------------------------------------------------------------------------
// Case model
// ---------------------------------------------------------------------------

/// A failure injection applied to the flow's first service task (E6-3
/// recovery cases). See the module docs for the exact semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Injection {
    /// Fail the first attempt transiently (E3-2 `transient`): the engine
    /// decrements retries, redelivers, and the second attempt succeeds.
    TransientFailOnce,
    /// Fail every attempt permanently (E3-2 `permanent`): straight to
    /// `Failed` + dead-letter queue, no retry storm.
    PermanentFail,
    /// Abort mid-handle past the lease (no outcome submitted): the lease
    /// expires, the engine redelivers, the second attempt completes.
    CrashAndRedeliver,
}

/// What a regression case expects of the v1 (baseline) definition.
///
/// The expectations describe the INTENDED behavior; a v2 run that misses
/// them is the regression the comparison surfaces (`expectation_met` in
/// [`CaseRun`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseExpectations {
    /// Whether the instance must reach `COMPLETED` (`false` = the case is
    /// legitimately blocked, e.g. a fail-closed approval gate).
    pub must_complete: bool,
    /// The end nodes the flow must have reached (empty = no constraint
    /// beyond the completion flag).
    pub expected_terminal_nodes: Vec<String>,
}

impl CaseExpectations {
    /// The flow must complete (no terminal-node constraint).
    pub fn completes() -> Self {
        Self {
            must_complete: true,
            expected_terminal_nodes: Vec::new(),
        }
    }

    /// The flow must NOT complete (a legitimately blocked case).
    pub fn blocked() -> Self {
        Self {
            must_complete: false,
            expected_terminal_nodes: Vec::new(),
        }
    }

    /// Builder: require these end nodes.
    pub fn terminal_nodes(mut self, nodes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.expected_terminal_nodes = nodes.into_iter().map(Into::into).collect();
        self
    }
}

/// One regression case: the flow (DSL spec plus its start variables and
/// user-task decisions — the `FlowSpec` carries both), the deterministic
/// expectation, and an optional failure injection.
#[derive(Debug, Clone)]
pub struct RegressionCase {
    /// Unique case name (report key).
    pub name: String,
    /// The v1 flow spec: DSL document, task types, start variables,
    /// user-task decisions.
    pub flow: FlowSpec,
    /// The deterministic expectation of the v1 behavior.
    pub expectations: CaseExpectations,
    /// Failure injection on the flow's first service task (`None` = clean
    /// run).
    pub inject: Option<Injection>,
}

impl RegressionCase {
    /// A case named `name` running `flow`, with the given completion
    /// expectation.
    pub fn new(name: impl Into<String>, flow: FlowSpec, must_complete: bool) -> Self {
        let expectations = if must_complete {
            CaseExpectations::completes()
        } else {
            CaseExpectations::blocked()
        };
        Self {
            name: name.into(),
            flow,
            expectations,
            inject: None,
        }
    }

    /// Builder: require these end nodes.
    pub fn terminal_nodes(mut self, nodes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.expectations = self.expectations.terminal_nodes(nodes);
        self
    }

    /// Builder: inject a failure on the flow's first service task.
    pub fn inject(mut self, injection: Injection) -> Self {
        self.inject = Some(injection);
        self
    }
}

// ---------------------------------------------------------------------------
// The fixed case set
// ---------------------------------------------------------------------------

/// The fixed sample set (E6-3 交付: 固定样例集): the five reference flows
/// plus deliberate variants. Thirteen cases, each run against both
/// definition versions:
///
/// | case | flow | variant / injection | expects |
/// |---|---|---|---|
/// | `retrieval-baseline` | 检索 | — | completes, `end` |
/// | `retrieval-transient-failure` | 检索 | [`Injection::TransientFailOnce`] on `ref-retrieve` | completes after one redelivery |
/// | `retrieval-worker-crash` | 检索 | [`Injection::CrashAndRedeliver`] on `ref-retrieve` | completes after lease-expiry redelivery |
/// | `retrieval-permanent-failure` | 检索 | [`Injection::PermanentFail`] on `ref-retrieve` | blocked, DLQ, 1 attempt |
/// | `inference-low-risk` | 推理 | `risk = low` | completes, `end-approved` |
/// | `inference-high-risk` | 推理 | `risk = high` | completes, `end-rejected` |
/// | `approval-approved` | 审批 | human approves | completes, `end`, 2 intervention points |
/// | `approval-unapproved` | 审批 | human rejects | blocked at `act`, DLQ |
/// | `tool-approved` | 工具 | `__approval` present | completes, `end` |
/// | `tool-transient-failure` | 工具 | [`Injection::TransientFailOnce`] on `ref-run-tool` | v1 completes after retry; **v2 (`retries: 1`) exhausts and DLQs — the behavior-changing fixture delta** |
/// | `tool-unapproved` | 工具 | no `__approval` | blocked, DLQ |
/// | `compensation-happy` | 补偿 | `saga_fail = false` | completes, `end-ok` |
/// | `compensation-failed` | 补偿 | `saga_fail = true` | completes, `end-compensated` |
pub fn fixed_case_set() -> Vec<RegressionCase> {
    use crate::flows::{
        approval_flow, compensation_flow, inference_flow, retrieval_flow, tool_flow,
    };
    vec![
        // 检索 retrieval: the baseline plus the three recovery injections.
        RegressionCase::new("retrieval-baseline", retrieval_flow(), true).terminal_nodes(["end"]),
        RegressionCase::new("retrieval-transient-failure", retrieval_flow(), true)
            .terminal_nodes(["end"])
            .inject(Injection::TransientFailOnce),
        RegressionCase::new("retrieval-worker-crash", retrieval_flow(), true)
            .terminal_nodes(["end"])
            .inject(Injection::CrashAndRedeliver),
        RegressionCase::new("retrieval-permanent-failure", retrieval_flow(), false)
            .inject(Injection::PermanentFail),
        // 推理 inference: both verdicts of the mock classifier.
        RegressionCase::new("inference-low-risk", inference_flow("low"), true)
            .terminal_nodes(["end-approved"]),
        RegressionCase::new("inference-high-risk", inference_flow("high"), true)
            .terminal_nodes(["end-rejected"]),
        // 审批 approval: the human decision both ways.
        RegressionCase::new("approval-approved", approval_flow(true), true).terminal_nodes(["end"]),
        RegressionCase::new("approval-unapproved", approval_flow(false), false),
        // 工具 tool: approved, unapproved, and the transient-retry case
        // whose v2 fixture (retries: 1) is the behavior-changing delta.
        RegressionCase::new(
            "tool-approved",
            tool_flow("quarterly close entry", true),
            true,
        )
        .terminal_nodes(["end"]),
        RegressionCase::new(
            "tool-transient-failure",
            tool_flow("transient retry memo", true),
            true,
        )
        .terminal_nodes(["end"])
        .inject(Injection::TransientFailOnce),
        RegressionCase::new(
            "tool-unapproved",
            tool_flow("memo-no-approval", false),
            false,
        ),
        // 补偿 compensation: happy path and the compensated failure path.
        RegressionCase::new("compensation-happy", compensation_flow(false), true)
            .terminal_nodes(["end-ok"]),
        RegressionCase::new("compensation-failed", compensation_flow(true), true)
            .terminal_nodes(["end-compensated"]),
    ]
}

// ---------------------------------------------------------------------------
// The v2 fixture definitions (the "definition versions" compared)
// ---------------------------------------------------------------------------

/// Retrieval v2 (definition change): an added `validate` step after the
/// model summary (`retrieve → summarize → validate → end`).
pub const RETRIEVAL_V2_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-retrieval:2", "name": "AI Reference: Retrieval (v2 fixture)" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "retrieve", "type": "serviceTask", "task_type": "ref-retrieve", "name": "Retrieve documents" },
    { "id": "summarize", "type": "serviceTask", "task_type": "ref-summarize", "name": "Summarize with model" },
    { "id": "validate", "type": "serviceTask", "task_type": "ref-validate", "name": "Validate the summary (added in v2)" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "retrieve" },
    { "source": "retrieve", "target": "summarize" },
    { "source": "summarize", "target": "validate" },
    { "source": "validate", "target": "end" }
  ]
}"#;

/// Inference v2 (definition change): the gateway default flipped — the
/// conditional matches `rejected` and the default approves. A
/// behavior-preserving refactor of the routing: both risk levels still
/// reach the same ends (through the other branch mechanism).
pub const INFERENCE_V2_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-inference:2", "name": "AI Reference: Inference (v2 fixture)" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "classify", "type": "serviceTask", "task_type": "ref-classify", "name": "Classify with model" },
    { "id": "decide", "type": "exclusiveGateway", "name": "Route on verdict (default flipped in v2)" },
    { "id": "act-approve", "type": "serviceTask", "task_type": "ref-act-approve", "name": "Approve" },
    { "id": "act-reject", "type": "serviceTask", "task_type": "ref-act-reject", "name": "Reject" },
    { "id": "end-approved", "type": "end" },
    { "id": "end-rejected", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "classify" },
    { "source": "classify", "target": "decide" },
    {
      "source": "decide",
      "target": "act-reject",
      "condition": { "kind": "variableEq", "variable": "verdict", "value": "rejected" }
    },
    { "source": "decide", "target": "act-approve", "default": true },
    { "source": "act-approve", "target": "end-approved" },
    { "source": "act-reject", "target": "end-rejected" }
  ]
}"#;

/// Approval v2 (definition change): an added `validate` step between the
/// human approval and the side-effect tool
/// (`propose → approve → validate → act → end`).
pub const APPROVAL_V2_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-approval:2", "name": "AI Reference: Approval (v2 fixture)" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "propose", "type": "serviceTask", "task_type": "ref-propose", "name": "Propose with model" },
    {
      "id": "approve",
      "type": "userTask",
      "name": "Human approval",
      "form_key": "approval-form",
      "form_fields": [
        {
          "id": "decision",
          "label": "Decision",
          "type": "enum",
          "required": true,
          "options": ["approved", "rejected"]
        }
      ]
    },
    { "id": "validate", "type": "serviceTask", "task_type": "ref-validate", "name": "Validate the proposal (added in v2)" },
    { "id": "act", "type": "serviceTask", "task_type": "ref-act", "name": "Execute approved action" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "propose" },
    { "source": "propose", "target": "approve" },
    { "source": "approve", "target": "validate" },
    { "source": "validate", "target": "act" },
    { "source": "act", "target": "end" }
  ]
}"#;

/// Tool v2 (definition change — **behavior-changing**): `retries` tightened
/// from the default 3 to 1, so a single transient failure exhausts the
/// budget (Failed + DLQ) instead of recovering on the redelivered attempt.
pub const TOOL_V2_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-tool:2", "name": "AI Reference: Tool (v2 fixture, retries tightened)" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "run-tool", "type": "serviceTask", "task_type": "ref-run-tool", "retries": 1, "name": "Post to ledger via tool" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "run-tool" },
    { "source": "run-tool", "target": "end" }
  ]
}"#;

/// Compensation v2 (definition change): the compensated path audits after
/// undoing (`… gate → undo → audit → end-compensated`); the happy path is
/// unchanged.
pub const COMPENSATION_V2_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-compensation:2", "name": "AI Reference: Saga Compensation (v2 fixture)" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "reserve", "type": "serviceTask", "task_type": "ref-saga-reserve", "name": "Reserve stock" },
    { "id": "charge", "type": "serviceTask", "task_type": "ref-saga-charge", "name": "Charge payment" },
    { "id": "saga-gate", "type": "exclusiveGateway", "name": "Saga outcome" },
    { "id": "undo", "type": "serviceTask", "task_type": "ref-saga-compensate", "name": "Compensate (reverse order)" },
    { "id": "audit", "type": "serviceTask", "task_type": "ref-validate", "name": "Audit the compensation (added in v2)" },
    { "id": "ship", "type": "serviceTask", "task_type": "ref-saga-ship", "name": "Ship order" },
    { "id": "end-compensated", "type": "end" },
    { "id": "end-ok", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "reserve" },
    { "source": "reserve", "target": "charge" },
    { "source": "charge", "target": "saga-gate" },
    {
      "source": "saga-gate",
      "target": "undo",
      "condition": { "kind": "variableEq", "variable": "saga_outcome", "value": "failed" }
    },
    { "source": "saga-gate", "target": "ship", "default": true },
    { "source": "undo", "target": "audit" },
    { "source": "audit", "target": "end-compensated" },
    { "source": "ship", "target": "end-ok" }
  ]
}"#;

/// Build the v2 variant of `case`'s flow: the v2 DSL, the `:2` definition
/// id, the v2 task types — with the case's own start variables and
/// user-task decisions grafted on (the SAME case input runs against both
/// versions; only the definition differs).
fn v2_variant(case: &RegressionCase) -> FlowSpec {
    let (name, dsl, task_types): (&str, &str, Vec<&str>) = match case.flow.process_def_id.as_str() {
        "ai-reference-retrieval" => (
            "retrieval-v2",
            RETRIEVAL_V2_DSL,
            vec![
                task_type::RETRIEVE,
                task_type::SUMMARIZE,
                task_type::VALIDATE,
            ],
        ),
        "ai-reference-inference" => (
            "inference-v2",
            INFERENCE_V2_DSL,
            vec![
                task_type::CLASSIFY,
                task_type::ACT_APPROVE,
                task_type::ACT_REJECT,
            ],
        ),
        "ai-reference-approval" => (
            "approval-v2",
            APPROVAL_V2_DSL,
            vec![task_type::PROPOSE, task_type::ACT, task_type::VALIDATE],
        ),
        "ai-reference-tool" => ("tool-v2", TOOL_V2_DSL, vec![task_type::RUN_TOOL]),
        "ai-reference-compensation" => (
            "compensation-v2",
            COMPENSATION_V2_DSL,
            vec![
                task_type::SAGA_RESERVE,
                task_type::SAGA_CHARGE,
                task_type::SAGA_COMPENSATE,
                task_type::SAGA_SHIP,
                task_type::VALIDATE,
            ],
        ),
        // Unknown flow: the honest identity fallback — the comparison then
        // reports no delta for it (documented; extend the table or pass a
        // custom selector).
        _ => return case.flow.clone(),
    };
    let definition_id = format!("{}:2", case.flow.process_def_id);
    let mut spec = FlowSpec::new(name, definition_id, dsl, task_types);
    spec.start_variables = case.flow.start_variables.clone();
    spec.user_decisions = case.flow.user_decisions.clone();
    spec
}

/// The fixture version selector for the five reference flows: v1 = the
/// case's own flow, v2 = the fixture variant (see the module docs).
pub fn fixture_version_selector() -> VersionSelector {
    Arc::new(|case, version| match version {
        Version::V1 => case.flow.clone(),
        Version::V2 => v2_variant(case),
    })
}

// ---------------------------------------------------------------------------
// The report model
// ---------------------------------------------------------------------------

/// The recovery verdict of one injected case run (deterministic — the
/// rules are testable; see `docs/regression-eval.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryVerdict {
    /// The injection that was applied.
    pub injection: Injection,
    /// Lease attempts on the injected node (`ExternalTaskLocked` count).
    pub node_attempts: u64,
    /// Failed attempts on the injected node (`ExternalTaskFailed` count).
    pub node_failed_attempts: u64,
    /// Whether the injected node's work eventually resolved.
    pub node_completed: bool,
    /// Whether a dead-letter entry exists for the instance.
    pub dlq_entry: bool,
    /// The overall verdict: transient/crash → completed via exactly one
    /// redelivery (crash: without a fail event); permanent → blocked with
    /// exactly one attempt and a DLQ entry (no retry storm).
    pub correct: bool,
}

/// One case run against one definition version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseRun {
    /// The case name (report key).
    pub case: String,
    /// The definition version label (`"v1"` / `"v2"`).
    pub version: String,
    /// The definition id the instance ran against.
    pub process_def_id: String,
    /// The instance id (opaque; differs per run).
    pub instance_id: String,
    /// Whether the instance reached `COMPLETED`.
    pub completed: bool,
    /// The end nodes the flow reached (steps whose node id starts with
    /// `end` — a token arriving at an end node completes the instance, so
    /// the step's existence is the reached signal).
    pub terminal_nodes: Vec<String>,
    /// Step count of the run — the deterministic latency proxy.
    pub steps: u64,
    /// Total lease attempts across all external-task steps — the
    /// deterministic retry/redelivery proxy.
    pub attempts: u64,
    /// Failed attempts across all external-task steps.
    pub failed_attempts: u64,
    /// Wall-clock latency of driving the case to its outcome — REPORTED
    /// ONLY, never asserted.
    pub latency_ms: u64,
    /// Sum of the E6-1 envelope costs (micro units of the pricing
    /// currency); deterministic given the mock provider's price table.
    pub cost_micros: u64,
    /// Sum of the envelope input-token counts.
    pub tokens_in: u64,
    /// Sum of the envelope output-token counts.
    pub tokens_out: u64,
    /// Human intervention points consumed: user-task decisions submitted +
    /// `__approval` inputs required.
    pub intervention_points: u64,
    /// Human decision points offered: declared user-task decisions +
    /// side-effect gates (`ref-act` / `ref-run-tool` task types).
    pub decision_points: u64,
    /// Whether a dead-letter entry exists for the instance.
    pub dlq_entry: bool,
    /// The recovery verdict (injected cases only).
    pub recovery: Option<RecoveryVerdict>,
    /// Whether the case's deterministic expectation was met (completed ==
    /// `must_complete` and every expected terminal node was reached).
    pub expectation_met: bool,
}

/// Per-version aggregates over a case set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionSummary {
    /// The version label.
    pub version: String,
    /// Number of cases run.
    pub cases: u64,
    /// Completed cases.
    pub completed: u64,
    /// `completed / cases` (0.0 for an empty set).
    pub success_rate: f64,
    /// Cases carrying a failure injection.
    pub injection_cases: u64,
    /// Injection cases with a correct recovery verdict.
    pub recovery_correct: u64,
    /// `recovery_correct / injection_cases`; `None` when no injection case
    /// ran.
    pub recovery_rate: Option<f64>,
    /// Intervention points consumed (see [`CaseRun::intervention_points`]).
    pub intervention_points: u64,
    /// Decision points offered (see [`CaseRun::decision_points`]).
    pub decision_points: u64,
    /// `intervention_points / decision_points` (0.0 when no decision
    /// points exist).
    pub intervention_rate: f64,
    /// Total envelope cost in micro units.
    pub total_cost_micros: u64,
    /// Total envelope input tokens.
    pub total_tokens_in: u64,
    /// Total envelope output tokens.
    pub total_tokens_out: u64,
    /// Total steps across all cases.
    pub total_steps: u64,
    /// Total lease attempts across all cases.
    pub total_attempts: u64,
    /// Total failed attempts across all cases.
    pub total_failed_attempts: u64,
    /// Cases whose deterministic expectation was met.
    pub expectations_met: u64,
    /// Mean wall-clock latency (ms) — reported only.
    pub mean_latency_ms: u64,
    /// Minimum wall-clock latency (ms) — reported only.
    pub min_latency_ms: u64,
    /// Maximum wall-clock latency (ms) — reported only.
    pub max_latency_ms: u64,
}

/// Per-case v1-vs-v2 comparison row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseDelta {
    /// The case name.
    pub case: String,
    /// v1 completed?
    pub completed_v1: bool,
    /// v2 completed?
    pub completed_v2: bool,
    /// v1 step count.
    pub steps_v1: u64,
    /// v2 step count.
    pub steps_v2: u64,
    /// v1 attempt count.
    pub attempts_v1: u64,
    /// v2 attempt count.
    pub attempts_v2: u64,
    /// v1 envelope cost (micro units).
    pub cost_v1: u64,
    /// v2 envelope cost (micro units).
    pub cost_v2: u64,
    /// v1 wall-clock latency (ms) — reported only.
    pub latency_v1: u64,
    /// v2 wall-clock latency (ms) — reported only.
    pub latency_v2: u64,
    /// v1 recovery verdict (injected cases only).
    pub recovered_v1: Option<bool>,
    /// v2 recovery verdict (injected cases only).
    pub recovered_v2: Option<bool>,
}

/// The v1 (baseline) → v2 (candidate) comparison of a case set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionComparison {
    /// The baseline version label.
    pub baseline: String,
    /// The candidate version label.
    pub candidate: String,
    /// `success_rate(candidate) - success_rate(baseline)`.
    pub success_rate_delta: f64,
    /// `recovery_rate(candidate) - recovery_rate(baseline)`; `None` when
    /// either side has no injection case.
    pub recovery_rate_delta: Option<f64>,
    /// `intervention_rate(candidate) - intervention_rate(baseline)`.
    pub intervention_rate_delta: f64,
    /// Total cost delta in micro units (candidate − baseline).
    pub cost_delta_micros: i64,
    /// Total steps delta (candidate − baseline).
    pub steps_delta: i64,
    /// Total attempts delta (candidate − baseline).
    pub attempts_delta: i64,
    /// Mean latency delta in ms (candidate − baseline) — reported only.
    pub mean_latency_delta_ms: i64,
    /// Per-case rows.
    pub per_case: Vec<CaseDelta>,
}

/// The full regression report: per case × version runs, per-version
/// aggregates and the v1 → v2 comparison. Serde-serializable (the example
/// harness writes it as `regression-report.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegressionReport {
    /// Harness identity string (recorded in the JSON artifact).
    pub harness: String,
    /// Number of cases in the set.
    pub cases: u64,
    /// The version labels that ran, in order.
    pub versions: Vec<String>,
    /// One row per case × version.
    pub per_case: Vec<CaseRun>,
    /// One summary per version (same order as `versions`).
    pub summary: Vec<VersionSummary>,
    /// The baseline → candidate comparison.
    pub comparison: VersionComparison,
}

// ---------------------------------------------------------------------------
// Metric extraction (pure: FlowOutcome -> metrics)
// ---------------------------------------------------------------------------

/// Error code prefix of the transient injection (assertable in attempt
/// errors and dead-letter entries).
pub const INJECTED_TRANSIENT_FAILURE: &str = "injected_transient_failure";

/// Error code prefix of the permanent injection.
pub const INJECTED_PERMANENT_FAILURE: &str = "injected_permanent_failure";

/// The node id and task type of the flow's FIRST `serviceTask` node (the
/// injection target), parsed from the DSL document.
fn injection_target(flow: &FlowSpec) -> Option<(String, String)> {
    let doc: Value = serde_json::from_str(&flow.dsl_json).ok()?;
    let nodes = doc.get("nodes")?.as_array()?;
    for node in nodes {
        if node.get("type").and_then(Value::as_str) == Some("serviceTask") {
            let id = node.get("id")?.as_str()?.to_string();
            let task_type = node.get("task_type")?.as_str()?.to_string();
            return Some((id, task_type));
        }
    }
    None
}

/// How long the crash injection sleeps past the lease: twice the lease
/// plus 500 ms — comfortably beyond the redelivery+completion window
/// (lease + one worker poll + the instant mock handler), so the crashed
/// attempt's stale submission is always rejected deterministically.
fn crash_sleep(lease: Duration) -> Duration {
    lease.saturating_mul(2) + Duration::from_millis(500)
}

/// Count occurrences of one event type in a step's event list.
fn count_events(step: &StepSummary, event: &str) -> u64 {
    step.event_types
        .iter()
        .filter(|e| e.as_str() == event)
        .count() as u64
}

/// The end nodes the flow reached. A token ARRIVING at an end node
/// completes the instance immediately (the engine's `End` handling emits
/// no per-token completion event), so a step whose node id starts with
/// `end` existing at all is the "reached" signal — the fixture flows' end
/// nodes are all `end*`.
fn terminal_nodes(outcome: &FlowOutcome) -> Vec<String> {
    outcome
        .steps
        .iter()
        .filter(|step| step.node_id.starts_with("end"))
        .map(|step| step.node_id.clone())
        .collect()
}

/// (intervention points, decision points) of a flow spec — deterministic,
/// derived from the definition, never from a clock.
fn intervention_counts(flow: &FlowSpec) -> (u64, u64) {
    let user = flow.user_decisions.len() as u64;
    let approval = u64::from(flow.start_variables.contains_key("__approval"));
    let gates = flow
        .task_types
        .iter()
        .filter(|t| t.as_str() == task_type::ACT || t.as_str() == task_type::RUN_TOOL)
        .count() as u64;
    (user + approval, user + gates)
}

/// The (cost micros, tokens in, tokens out) recorded in a step's E6-1
/// envelope summary (0/0/0 when the step recorded no envelope cost).
fn step_cost(step: &StepSummary) -> (u64, u64, u64) {
    let Some(envelope) = &step.envelope else {
        return (0, 0, 0);
    };
    let cost = envelope.get("cost").cloned().unwrap_or(Value::Null);
    (
        cost.get("amount_micros")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cost.get("tokens_in")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cost.get("tokens_out")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    )
}

/// The recovery verdict of an injected run (pure — the deterministic rules
/// the tests can pin).
fn recovery_verdict(
    injection: Injection,
    completed: bool,
    node_attempts: u64,
    node_failed_attempts: u64,
    node_completed: bool,
    dlq_entry: bool,
) -> RecoveryVerdict {
    let correct = match injection {
        // Transient: the redelivered attempt completed the flow — exactly
        // two attempts (one failed, one completed), nothing in the DLQ.
        Injection::TransientFailOnce => {
            completed && node_attempts == 2 && node_failed_attempts == 1 && !dlq_entry
        }
        // Crash: the lease expired (NOT a fail event — no retry consumed),
        // the redelivered attempt completed the flow: two attempts, zero
        // failures, nothing in the DLQ.
        Injection::CrashAndRedeliver => {
            completed && node_attempts == 2 && node_failed_attempts == 0 && !dlq_entry
        }
        // Permanent: no retry storm — exactly ONE attempt, terminally
        // failed into the DLQ, the instance blocked.
        Injection::PermanentFail => {
            !completed && node_attempts == 1 && node_failed_attempts == 1 && dlq_entry
        }
    };
    RecoveryVerdict {
        injection,
        node_attempts,
        node_failed_attempts,
        node_completed,
        dlq_entry,
        correct,
    }
}

/// Assemble one [`CaseRun`] from the run's measured outcome (pure).
fn assemble_case_run(
    case: &RegressionCase,
    version: Version,
    flow: &FlowSpec,
    outcome: &FlowOutcome,
    latency_ms: u64,
    dlq_entry: bool,
) -> CaseRun {
    let (intervention_points, decision_points) = intervention_counts(flow);
    let (mut cost_micros, mut tokens_in, mut tokens_out) = (0u64, 0u64, 0u64);
    let (mut attempts, mut failed_attempts) = (0u64, 0u64);
    for step in &outcome.steps {
        attempts += count_events(step, "ExternalTaskLocked");
        failed_attempts += count_events(step, "ExternalTaskFailed");
        let (micros, tin, tout) = step_cost(step);
        cost_micros += micros;
        tokens_in += tin;
        tokens_out += tout;
    }
    let completed = outcome.is_completed();
    let terminals = terminal_nodes(outcome);
    let recovery = case.inject.map(|injection| {
        let (node, _) = injection_target(flow).unwrap_or_default();
        let node_step = outcome.step(&node);
        let node_attempts = node_step
            .map(|step| count_events(step, "ExternalTaskLocked"))
            .unwrap_or_default();
        let node_failed = node_step
            .map(|step| count_events(step, "ExternalTaskFailed"))
            .unwrap_or_default();
        let node_completed = node_step.is_some_and(StepSummary::executed);
        recovery_verdict(
            injection,
            completed,
            node_attempts,
            node_failed,
            node_completed,
            dlq_entry,
        )
    });
    let terminals_met = case
        .expectations
        .expected_terminal_nodes
        .iter()
        .all(|expected| terminals.contains(expected));
    let expectation_met = (completed == case.expectations.must_complete) && terminals_met;
    CaseRun {
        case: case.name.clone(),
        version: version.label().to_string(),
        process_def_id: flow.process_def_id.clone(),
        instance_id: outcome.instance_id.clone(),
        completed,
        terminal_nodes: terminals,
        steps: outcome.steps.len() as u64,
        attempts,
        failed_attempts,
        latency_ms,
        cost_micros,
        tokens_in,
        tokens_out,
        intervention_points,
        decision_points,
        dlq_entry,
        recovery,
        expectation_met,
    }
}

// ---------------------------------------------------------------------------
// Aggregation (pure: CaseRuns -> summaries/comparison)
// ---------------------------------------------------------------------------

/// Aggregate the runs of one version into a [`VersionSummary`] (pure; the
/// unit tests pin the math).
fn summarize(version: &str, runs: &[CaseRun]) -> VersionSummary {
    let cases = runs.len() as u64;
    let completed = runs.iter().filter(|r| r.completed).count() as u64;
    let injected: Vec<&CaseRun> = runs.iter().filter(|r| r.recovery.is_some()).collect();
    let injection_cases = injected.len() as u64;
    let recovery_correct = injected
        .iter()
        .filter(|r| r.recovery.as_ref().is_some_and(|v| v.correct))
        .count() as u64;
    let latencies: Vec<u64> = runs.iter().map(|r| r.latency_ms).collect();
    VersionSummary {
        version: version.to_string(),
        cases,
        completed,
        success_rate: if cases == 0 {
            0.0
        } else {
            completed as f64 / cases as f64
        },
        injection_cases,
        recovery_correct,
        recovery_rate: if injection_cases == 0 {
            None
        } else {
            Some(recovery_correct as f64 / injection_cases as f64)
        },
        intervention_points: runs.iter().map(|r| r.intervention_points).sum(),
        decision_points: runs.iter().map(|r| r.decision_points).sum(),
        intervention_rate: {
            let points: u64 = runs.iter().map(|r| r.intervention_points).sum();
            let decisions: u64 = runs.iter().map(|r| r.decision_points).sum();
            if decisions == 0 {
                0.0
            } else {
                points as f64 / decisions as f64
            }
        },
        total_cost_micros: runs.iter().map(|r| r.cost_micros).sum(),
        total_tokens_in: runs.iter().map(|r| r.tokens_in).sum(),
        total_tokens_out: runs.iter().map(|r| r.tokens_out).sum(),
        total_steps: runs.iter().map(|r| r.steps).sum(),
        total_attempts: runs.iter().map(|r| r.attempts).sum(),
        total_failed_attempts: runs.iter().map(|r| r.failed_attempts).sum(),
        expectations_met: runs.iter().filter(|r| r.expectation_met).count() as u64,
        // checked_div: an empty set sums to a mean of 0, never a panic.
        mean_latency_ms: latencies
            .iter()
            .sum::<u64>()
            .checked_div(cases)
            .unwrap_or_default(),
        min_latency_ms: latencies.iter().copied().min().unwrap_or_default(),
        max_latency_ms: latencies.iter().copied().max().unwrap_or_default(),
    }
}

/// The v1 → v2 comparison (pure). `v1_runs` / `v2_runs` must be keyed by
/// the same case names (the harness guarantees that — the same case set
/// runs against both versions).
fn compare(
    v1: &VersionSummary,
    v2: &VersionSummary,
    v1_runs: &[CaseRun],
    v2_runs: &[CaseRun],
) -> VersionComparison {
    let mut per_case = Vec::with_capacity(v1_runs.len());
    for run1 in v1_runs {
        let Some(run2) = v2_runs.iter().find(|r| r.case == run1.case) else {
            continue;
        };
        per_case.push(CaseDelta {
            case: run1.case.clone(),
            completed_v1: run1.completed,
            completed_v2: run2.completed,
            steps_v1: run1.steps,
            steps_v2: run2.steps,
            attempts_v1: run1.attempts,
            attempts_v2: run2.attempts,
            cost_v1: run1.cost_micros,
            cost_v2: run2.cost_micros,
            latency_v1: run1.latency_ms,
            latency_v2: run2.latency_ms,
            recovered_v1: run1.recovery.as_ref().map(|v| v.correct),
            recovered_v2: run2.recovery.as_ref().map(|v| v.correct),
        });
    }
    VersionComparison {
        baseline: v1.version.clone(),
        candidate: v2.version.clone(),
        success_rate_delta: v2.success_rate - v1.success_rate,
        recovery_rate_delta: match (v1.recovery_rate, v2.recovery_rate) {
            (Some(a), Some(b)) => Some(b - a),
            _ => None,
        },
        intervention_rate_delta: v2.intervention_rate - v1.intervention_rate,
        cost_delta_micros: v2.total_cost_micros as i64 - v1.total_cost_micros as i64,
        steps_delta: v2.total_steps as i64 - v1.total_steps as i64,
        attempts_delta: v2.total_attempts as i64 - v1.total_attempts as i64,
        mean_latency_delta_ms: v2.mean_latency_ms as i64 - v1.mean_latency_ms as i64,
        per_case,
    }
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/// The eval worker timing profile: a 2 s lease and a 200 ms worker poll.
///
/// The lease is 2 s (not 1 s) on purpose: the store's lease expiry is
/// SECOND-granular (`lock_expire_at = unix_secs() + lease_secs`), so a 1 s
/// lease is really 0–1000 ms of wall time depending on where inside the
/// second the lock lands — a plain task could then expire mid-handle and
/// be re-locked, adding phantom attempts. 2 s guarantees a ≥1 s true
/// lease, which the instant mock handler never exceeds, while the crash
/// injection still redelivers quickly. Everything else inherits
/// [`FlowOptions::default`] (mock pricing, the 30 s run timeout — blocked
/// cases exit early on their dead-letter entry instead of waiting it
/// out).
pub fn eval_flow_options() -> FlowOptions {
    FlowOptions {
        worker_id: "eval-worker".to_string(),
        worker_poll_interval: Duration::from_millis(200),
        lock_duration: Duration::from_secs(2),
        ..FlowOptions::default()
    }
}

/// Per-run state of the injection hook: the task ids the injection has
/// already fired for (so a redelivered attempt runs the real handler).
#[derive(Default)]
struct InjectionState {
    fired: Mutex<HashSet<String>>,
}

/// The worker-side injection hook (E6-3 failure injection): wraps the real
/// [`ReferenceHandler`] and applies the case's [`Injection`] to the target
/// task type; every other task (and every redelivered attempt after the
/// injection fired) delegates to the real handler.
struct InjectingHandler {
    declared: String,
    inner: ReferenceHandler,
    injection: Option<Injection>,
    target_task_type: Option<String>,
    state: Arc<InjectionState>,
}

impl InjectingHandler {
    fn new(
        declared: impl Into<String>,
        inner: ReferenceHandler,
        injection: Option<Injection>,
        target_task_type: Option<String>,
        state: Arc<InjectionState>,
    ) -> Self {
        Self {
            declared: declared.into(),
            inner,
            injection,
            target_task_type,
            state,
        }
    }

    /// Whether this task invocation triggers the injection.
    fn triggers(&self, task: &ExternalTask) -> Option<Injection> {
        let injection = self.injection?;
        if Some(task.task_type.as_str()) != self.target_task_type.as_deref() {
            return None;
        }
        match injection {
            // Permanent fails EVERY attempt (there is only ever one — the
            // task goes straight to Failed).
            Injection::PermanentFail => Some(injection),
            // The transient/crash injections fire once per task id: the
            // redelivered attempt does the real work.
            Injection::TransientFailOnce | Injection::CrashAndRedeliver => {
                let first = self
                    .state
                    .fired
                    .lock()
                    .map(|mut fired| fired.insert(task.task_id.clone()))
                    .unwrap_or(false);
                if first {
                    Some(injection)
                } else {
                    None
                }
            }
        }
    }
}

#[async_trait]
impl TaskHandler for InjectingHandler {
    fn task_type(&self) -> &str {
        &self.declared
    }

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
        match self.triggers(&task) {
            None => self.inner.handle(task, ctx).await,
            Some(Injection::PermanentFail) => TaskResult::Fail {
                error: format!(
                    "{INJECTED_PERMANENT_FAILURE}: E6-3 eval harness (permanent, \
                     straight to the dead letter queue)"
                ),
                retry_after: None,
                category: Some(RetryCategory::Permanent),
            },
            Some(Injection::TransientFailOnce) => TaskResult::Fail {
                error: format!(
                    "{INJECTED_TRANSIENT_FAILURE}: E6-3 eval harness (fail once, \
                     succeed on the redelivered attempt)"
                ),
                retry_after: None,
                category: Some(RetryCategory::Transient),
            },
            Some(Injection::CrashAndRedeliver) => {
                // Simulate a worker crash mid-handle: sleep PAST the lease
                // with no renewal (the real handler — and its auto-renewal —
                // has not started yet), then return a result whose
                // submission the engine rejects because the task was
                // redelivered and completed by another attempt in the
                // meantime (the stale-lease guard). No fail is submitted, so
                // no retry is consumed — exactly the engine's crash
                // semantics (E3-2).
                let sleep = crash_sleep(ctx.lock_duration());
                tokio::select! {
                    // Shutdown during the crash window: return the stale
                    // result immediately (its submission is rejected all
                    // the same — the lease is gone).
                    _ = ctx.cancelled() => {}
                    _ = tokio::time::sleep(sleep) => {}
                }
                TaskResult::Complete {
                    variables: HashMap::from([(
                        "crash_marker".to_string(),
                        "stale attempt after lease expiry".to_string(),
                    )]),
                }
            }
        }
    }
}

/// Run one case against one definition version and measure the outcome.
async fn run_case(
    server_url: &str,
    case: &RegressionCase,
    version: Version,
    flow: &FlowSpec,
    provider: &Arc<dyn ModelProvider>,
    options: &FlowOptions,
) -> anyhow::Result<CaseRun> {
    let rest = Rest::new(server_url);
    let client = EngineClient::new(server_url);

    // One dispatching injecting handler per task type of THIS version's
    // flow; the injection targets the version's first service task.
    let deps = Arc::new(HandlerDeps::new(Arc::clone(provider), options));
    let target = injection_target(flow).map(|(_, task_type)| task_type);
    let state = Arc::new(InjectionState::default());
    let worker_config = WorkerConfig::new(options.worker_id.clone())
        .poll_interval(options.worker_poll_interval)
        .lock_duration(options.lock_duration)
        .fetch_retry_max(0)
        .shutdown_grace(options.shutdown_grace);
    let mut builder = Worker::builder().client(client).config(worker_config);
    for task_type in &flow.task_types {
        let inner = ReferenceHandler::new(task_type.clone(), Arc::clone(&deps));
        builder = builder.handler(InjectingHandler::new(
            task_type.clone(),
            inner,
            case.inject,
            target.clone(),
            Arc::clone(&state),
        ));
    }
    let worker = builder.build();
    let handle = worker.spawn();

    // Drive the instance to its outcome; a drive failure still shuts the
    // worker down.
    let started = Instant::now();
    let driven = drive_case(&rest, flow, options).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let drained = handle.shutdown(options.shutdown_grace).await;
    let instance_id = driven?;
    if !drained {
        tracing::warn!(
            case = %case.name,
            version = version.label(),
            "eval worker did not drain within the grace (stragglers aborted)"
        );
    }

    let outcome = build_outcome(&rest, &flow.name, &instance_id)
        .await
        .with_context(|| format!("snapshotting case '{}'", case.name))?;
    let dlq_entry = rest.has_dead_letter(&instance_id).await?;
    Ok(assemble_case_run(
        case, version, flow, &outcome, latency_ms, dlq_entry,
    ))
}

/// Start the instance and poll until it reaches a terminal state, a
/// dead-letter entry for it appears (terminally blocked — the eval's early
/// exit for legitimately blocked cases), or the run timeout elapses
/// (blocked flows are outcomes, not errors).
async fn drive_case(rest: &Rest, flow: &FlowSpec, options: &FlowOptions) -> anyhow::Result<String> {
    let instance_id = rest
        .start_instance(&flow.process_def_id, &flow.start_variables)
        .await
        .with_context(|| format!("starting case flow '{}'", flow.name))?;
    let deadline = Instant::now() + options.timeout;
    let mut submitted: HashSet<String> = HashSet::new();

    loop {
        let instance = rest
            .get_instance(&instance_id)
            .await
            .with_context(|| format!("polling instance {instance_id}"))?;
        let state = instance
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state != "RUNNING" {
            return Ok(instance_id);
        }

        // Submit user-task decisions once their tokens arrive (same
        // mechanics as the flow runner: the task id is
        // "<instance_id>:<node_id>").
        let current_nodes = instance
            .get("current_nodes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for decision in &flow.user_decisions {
            if submitted.contains(&decision.node_id) {
                continue;
            }
            if current_nodes.iter().any(|n| n == &decision.node_id) {
                rest.complete_user_task(&instance_id, &decision.node_id, &decision.variables)
                    .await
                    .with_context(|| {
                        format!(
                            "completing user task '{}' of case flow '{}'",
                            decision.node_id, flow.name
                        )
                    })?;
                submitted.insert(decision.node_id.clone());
            }
        }

        // A dead-letter entry means the instance is terminally blocked in
        // this engine (its token stays at the failed node): stop waiting.
        if rest.has_dead_letter(&instance_id).await? {
            return Ok(instance_id);
        }

        if Instant::now() >= deadline {
            return Ok(instance_id);
        }
        tokio::time::sleep(options.poll_interval).await;
    }
}

/// Run the fixed case set against BOTH definition versions (v1 then v2)
/// and aggregate the report. Uses the eval timing profile
/// ([`eval_flow_options`]).
///
/// `deployer` is the spawned server's definition deployer
/// (`state.storage.deployer.as_ref()`): DSL flows can only be registered
/// in-process (the REST deploy endpoint accepts BPMN XML), so the harness
/// deploys every selected variant itself — deduplicated by definition id,
/// before any case runs.
pub async fn run_regression(
    server_url: &str,
    deployer: &dyn DefinitionDeployer,
    provider: Arc<dyn ModelProvider>,
    cases: &[RegressionCase],
    version_selector: &VersionSelector,
) -> anyhow::Result<RegressionReport> {
    run_regression_with(
        server_url,
        deployer,
        provider,
        cases,
        version_selector,
        eval_flow_options(),
    )
    .await
}

/// The tunable variant of [`run_regression`] (custom worker identity,
/// timeouts, lease, tool credential env var).
pub async fn run_regression_with(
    server_url: &str,
    deployer: &dyn DefinitionDeployer,
    provider: Arc<dyn ModelProvider>,
    cases: &[RegressionCase],
    version_selector: &VersionSelector,
    options: FlowOptions,
) -> anyhow::Result<RegressionReport> {
    // 1. Deploy every (case × version) variant once, deduplicated by
    //    definition id, before anything runs.
    let mut deployed: HashSet<String> = HashSet::new();
    for case in cases {
        for version in Version::all() {
            let flow = version_selector(case, version);
            if deployed.insert(flow.process_def_id.clone()) {
                deploy_flow(deployer, &flow)
                    .await
                    .with_context(|| format!("deploying '{}'", flow.process_def_id))?;
            }
        }
    }

    // 2. Run the case set against v1, then v2 (sequential: one worker at a
    //    time, deterministic attempt accounting).
    let mut per_case: Vec<CaseRun> = Vec::with_capacity(cases.len() * 2);
    for version in Version::all() {
        for case in cases {
            let flow = version_selector(case, version);
            let run = run_case(server_url, case, version, &flow, &provider, &options)
                .await
                .with_context(|| format!("running case '{}' ({})", case.name, version.label()))?;
            per_case.push(run);
        }
    }

    // 3. Aggregate.
    let v1_runs: Vec<CaseRun> = per_case
        .iter()
        .filter(|r| r.version == Version::V1.label())
        .cloned()
        .collect();
    let v2_runs: Vec<CaseRun> = per_case
        .iter()
        .filter(|r| r.version == Version::V2.label())
        .cloned()
        .collect();
    let summary_v1 = summarize(Version::V1.label(), &v1_runs);
    let summary_v2 = summarize(Version::V2.label(), &v2_runs);
    let comparison = compare(&summary_v1, &summary_v2, &v1_runs, &v2_runs);
    Ok(RegressionReport {
        harness: "bpm-engine-ai-reference regression eval (E6-3)".to_string(),
        cases: cases.len() as u64,
        versions: Version::all()
            .iter()
            .map(|v| v.label().to_string())
            .collect(),
        per_case,
        summary: vec![summary_v1, summary_v2],
        comparison,
    })
}

// ---------------------------------------------------------------------------
// Markdown summary
// ---------------------------------------------------------------------------

impl RegressionReport {
    /// A human-readable markdown rendering of the report: per-version
    /// metrics, the v1 → v2 deltas and the per-case × version table.
    ///
    /// Wall-clock latency is INCLUDED but explicitly marked reported-only.
    pub fn markdown_summary(&self) -> String {
        let mut out = String::new();
        out.push_str("# Regression Eval Report (E6-3)\n\n");
        out.push_str(&format!(
            "harness: {} — {} cases × {} definition versions, mock provider, \
             offline (loopback only).\n\n",
            self.harness,
            self.cases,
            self.versions.join(" vs ")
        ));

        out.push_str("## Per-version metrics\n\n");
        out.push_str("| version | success | recovery | intervention | cost µUSD | tokens (in/out) | steps | attempts | mean lat. ms |\n");
        out.push_str("|---|---|---|---|---|---|---|---|---|\n");
        for s in &self.summary {
            let recovery = s
                .recovery_rate
                .map(|r| format!("{r:.3}"))
                .unwrap_or_else(|| "—".to_string());
            out.push_str(&format!(
                "| {} | {:.3} | {} | {:.3} ({} / {}) | {} | {} / {} | {} | {} | {} |\n",
                s.version,
                s.success_rate,
                recovery,
                s.intervention_rate,
                s.intervention_points,
                s.decision_points,
                s.total_cost_micros,
                s.total_tokens_in,
                s.total_tokens_out,
                s.total_steps,
                s.total_attempts,
                s.mean_latency_ms
            ));
        }
        out.push_str(
            "\n(recovery = correct-recovery fraction over the injection cases; \
             intervention = points / decision points; latency is wall clock and \
             **reported only** — never asserted.)\n\n",
        );

        let c = &self.comparison;
        out.push_str(&format!(
            "## {} → {} deltas\n\n- success rate: {:+.3} ({:.3} → {:.3})\n",
            c.baseline,
            c.candidate,
            c.success_rate_delta,
            self.summary
                .first()
                .map(|s| s.success_rate)
                .unwrap_or_default(),
            self.summary
                .get(1)
                .map(|s| s.success_rate)
                .unwrap_or_default()
        ));
        if let Some(recovery_delta) = c.recovery_rate_delta {
            out.push_str(&format!("- recovery rate: {recovery_delta:+.3}\n"));
        }
        out.push_str(&format!(
            "- intervention rate: {:+.3}\n- cost: {:+} µUSD ({} → {})\n- steps: {:+} \
             ({} → {})\n- attempts: {:+} ({} → {})\n- mean latency: {:+} ms \
             (reported only)\n\n",
            c.intervention_rate_delta,
            c.cost_delta_micros,
            self.summary
                .first()
                .map(|s| s.total_cost_micros)
                .unwrap_or_default(),
            self.summary
                .get(1)
                .map(|s| s.total_cost_micros)
                .unwrap_or_default(),
            c.steps_delta,
            self.summary
                .first()
                .map(|s| s.total_steps)
                .unwrap_or_default(),
            self.summary
                .get(1)
                .map(|s| s.total_steps)
                .unwrap_or_default(),
            c.attempts_delta,
            self.summary
                .first()
                .map(|s| s.total_attempts)
                .unwrap_or_default(),
            self.summary
                .get(1)
                .map(|s| s.total_attempts)
                .unwrap_or_default(),
            c.mean_latency_delta_ms
        ));

        out.push_str("## Per-case × version\n\n");
        out.push_str(
            "| case | version | completed | terminal | steps | attempts | failed | \
             cost µUSD | lat. ms | recovery | dlq | met |\n",
        );
        out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");
        for r in &self.per_case {
            let recovery = r
                .recovery
                .as_ref()
                .map(|v| {
                    if v.correct {
                        "correct".to_string()
                    } else {
                        format!("WRONG ({} attempts)", v.node_attempts)
                    }
                })
                .unwrap_or_else(|| "—".to_string());
            out.push_str(&format!(
                "| {} | {} | {} | {:?} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r.case,
                r.version,
                if r.completed { "yes" } else { "no" },
                r.terminal_nodes,
                r.steps,
                r.attempts,
                r.failed_attempts,
                r.cost_micros,
                r.latency_ms,
                recovery,
                if r.dlq_entry { "yes" } else { "no" },
                if r.expectation_met { "yes" } else { "NO" }
            ));
        }
        out.push_str(
            "\n(latency is wall clock, reported only; every other column is a \
             deterministic, testable value.)\n",
        );
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::{retrieval_flow, tool_flow};
    use crate::provider::MockModelProvider;

    // ---- the fixed case set -------------------------------------------------

    #[test]
    fn fixed_case_set_is_thirteen_unique_cases() {
        let cases = fixed_case_set();
        assert_eq!(cases.len(), 13, "the fixed sample set");
        let mut names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), cases.len(), "case names must be unique");

        // The injection distribution: three recovery injections on the
        // retrieval flow, one transient on the tool flow.
        let injected: Vec<&str> = cases
            .iter()
            .filter_map(|c| c.inject.map(|_| c.name.as_str()))
            .collect();
        assert_eq!(
            injected,
            [
                "retrieval-transient-failure",
                "retrieval-worker-crash",
                "retrieval-permanent-failure",
                "tool-transient-failure"
            ]
        );
        // Every v1 flow compiles (a broken case would fail before any
        // server exists).
        for case in &cases {
            crate::flows::compile_flow(&case.flow)
                .unwrap_or_else(|e| panic!("case '{}' must compile: {e}", case.name));
        }
    }

    #[test]
    fn every_v2_fixture_dsl_document_compiles() {
        for (name, dsl) in [
            ("retrieval-v2", RETRIEVAL_V2_DSL),
            ("inference-v2", INFERENCE_V2_DSL),
            ("approval-v2", APPROVAL_V2_DSL),
            ("tool-v2", TOOL_V2_DSL),
            ("compensation-v2", COMPENSATION_V2_DSL),
        ] {
            let def = bpm_engine_dsl::compile(dsl)
                .unwrap_or_else(|report| panic!("v2 fixture '{name}' must compile: {report:?}"));
            assert!(!def.nodes.is_empty());
        }
    }

    #[test]
    fn fixture_selector_returns_versioned_variants() {
        let selector = fixture_version_selector();
        for case in fixed_case_set() {
            // v1: the case's own flow, verbatim.
            let v1 = selector(&case, Version::V1);
            assert_eq!(v1.process_def_id, case.flow.process_def_id);
            assert_eq!(v1.start_variables, case.flow.start_variables);

            let v2 = selector(&case, Version::V2);
            // The v2 definitions of the five reference flows are registered
            // under :2-suffixed ids.
            let expected_suffix = format!("{}:2", case.flow.process_def_id);
            if v2.process_def_id != case.flow.process_def_id {
                assert_eq!(v2.process_def_id, expected_suffix, "case {}", case.name);
                // The SAME case input runs against both versions: start
                // variables and user decisions are grafted.
                assert_eq!(v2.start_variables, case.flow.start_variables);
                assert_eq!(v2.user_decisions, case.flow.user_decisions);
                assert!(crate::flows::compile_flow(&v2).is_ok());
            }
        }
    }

    #[test]
    fn fixture_selector_identity_fallback_for_unknown_flows() {
        // A flow outside the fixture table: v2 == v1 (honest, documented).
        let selector = fixture_version_selector();
        let custom = crate::flows::FlowSpec::new(
            "custom",
            "custom-eval-flow",
            r#"{
              "schema_version": 1,
              "process": { "id": "custom-eval-flow", "name": "Custom" },
              "nodes": [
                { "id": "start", "type": "start" },
                { "id": "work", "type": "serviceTask", "task_type": "ref-retrieve" },
                { "id": "end", "type": "end" }
              ],
              "edges": [
                { "source": "start", "target": "work" },
                { "source": "work", "target": "end" }
              ]
            }"#,
            ["ref-retrieve"],
        );
        let case = RegressionCase::new("custom", custom, true);
        let v2 = selector(&case, Version::V2);
        assert_eq!(v2.process_def_id, case.flow.process_def_id);
    }

    #[test]
    fn tool_v2_fixture_tightens_retries_to_one() {
        // The one behavior-changing fixture: v1's run-tool node keeps the
        // DSL default (3 retries), v2 declares exactly 1.
        let selector = fixture_version_selector();
        let case = RegressionCase::new("tool", tool_flow("m", true), true);
        let v1 = selector(&case, Version::V1);
        let v2 = selector(&case, Version::V2);
        let retries_of = |flow: &FlowSpec| {
            let def = crate::flows::compile_flow(flow).expect("compiles");
            match &def.nodes.get("run-tool").expect("run-tool node").node_type {
                bpm_engine_core::NodeType::ExternalTask { retries, .. } => *retries,
                other => panic!("unexpected node type: {other:?}"),
            }
        };
        assert_eq!(retries_of(&v1), 3, "DSL default");
        assert_eq!(retries_of(&v2), 1, "the v2 tightening");
    }

    #[test]
    fn injection_target_is_the_first_service_task() {
        let retrieval = retrieval_flow();
        assert_eq!(
            injection_target(&retrieval),
            Some(("retrieve".to_string(), "ref-retrieve".to_string()))
        );
        let tool = tool_flow("m", true);
        assert_eq!(
            injection_target(&tool),
            Some(("run-tool".to_string(), "ref-run-tool".to_string()))
        );
        // A v2 variant of the retrieval flow still targets `retrieve` (the
        // first service task is unchanged).
        let selector = fixture_version_selector();
        let case = RegressionCase::new("r", retrieval_flow(), true);
        let v2 = selector(&case, Version::V2);
        assert_eq!(
            injection_target(&v2),
            Some(("retrieve".to_string(), "ref-retrieve".to_string()))
        );
    }

    // ---- pure metric helpers -------------------------------------------------

    #[test]
    fn crash_sleep_scales_with_and_exceeds_the_lease() {
        let lease = Duration::from_secs(1);
        let sleep = crash_sleep(lease);
        assert!(sleep > lease * 2, "must clear the redelivery window");
        assert_eq!(sleep, Duration::from_millis(2500));
        // Long leases keep the property.
        assert!(crash_sleep(Duration::from_secs(30)) > Duration::from_secs(60));
    }

    /// A synthetic `CaseRun` for aggregation-math tests (deterministic
    /// defaults: completed, one step/attempt, no cost, no latency, no
    /// intervention, no recovery, expectation met).
    fn synthetic_run(case: &str, version: &str) -> CaseRun {
        CaseRun {
            case: case.to_string(),
            version: version.to_string(),
            process_def_id: "def".to_string(),
            instance_id: "inst".to_string(),
            completed: true,
            terminal_nodes: vec![],
            steps: 1,
            attempts: 1,
            failed_attempts: 0,
            latency_ms: 0,
            cost_micros: 0,
            tokens_in: 0,
            tokens_out: 0,
            intervention_points: 0,
            decision_points: 0,
            dlq_entry: false,
            recovery: None,
            expectation_met: true,
        }
    }

    /// A synthetic `RecoveryVerdict` (one failed attempt by default).
    fn synthetic_verdict(correct: bool, node_attempts: u64) -> RecoveryVerdict {
        RecoveryVerdict {
            injection: Injection::TransientFailOnce,
            node_attempts,
            node_failed_attempts: 1,
            node_completed: correct,
            dlq_entry: false,
            correct,
        }
    }

    /// Apply overrides to a synthetic run.
    fn tune(mut run: CaseRun, f: impl FnOnce(&mut CaseRun)) -> CaseRun {
        f(&mut run);
        run
    }

    #[test]
    fn summarize_computes_rates_sums_and_extremes() {
        let runs = vec![
            tune(synthetic_run("a", "v1"), |r| {
                r.steps = 2;
                r.attempts = 2;
                r.cost_micros = 18;
                r.latency_ms = 100;
                r.intervention_points = 2;
                r.decision_points = 2;
                r.recovery = Some(synthetic_verdict(true, 2));
            }),
            tune(synthetic_run("b", "v1"), |r| {
                r.completed = false;
                r.cost_micros = 7;
                r.latency_ms = 300;
                r.intervention_points = 1;
                r.decision_points = 2;
                r.recovery = Some(synthetic_verdict(false, 1));
                r.expectation_met = false;
            }),
            tune(synthetic_run("c", "v1"), |r| {
                r.cost_micros = 8;
                r.latency_ms = 200;
            }),
        ];
        let s = summarize("v1", &runs);
        assert_eq!(s.cases, 3);
        assert_eq!(s.completed, 2);
        assert!((s.success_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(s.injection_cases, 2);
        assert_eq!(s.recovery_correct, 1);
        assert_eq!(s.recovery_rate, Some(0.5));
        assert_eq!(s.intervention_points, 3);
        assert_eq!(s.decision_points, 4);
        assert!((s.intervention_rate - 0.75).abs() < 1e-9);
        assert_eq!(s.total_cost_micros, 33);
        assert_eq!(s.total_steps, 4);
        assert_eq!(s.total_attempts, 4);
        assert_eq!(s.expectations_met, 2);
        // (100 + 300 + 200) / 3 = 200; min 100; max 300.
        assert_eq!(s.mean_latency_ms, 200);
        assert_eq!(s.min_latency_ms, 100);
        assert_eq!(s.max_latency_ms, 300);
    }

    #[test]
    fn summarize_of_an_empty_set_is_all_zeros() {
        let s = summarize("v1", &[]);
        assert_eq!(s.cases, 0);
        assert_eq!(s.success_rate, 0.0);
        assert_eq!(s.recovery_rate, None);
        assert_eq!(s.intervention_rate, 0.0);
        assert_eq!(s.mean_latency_ms, 0);
        assert_eq!(s.min_latency_ms, 0);
        assert_eq!(s.max_latency_ms, 0);
    }

    #[test]
    fn compare_computes_deltas_per_case() {
        let v1_runs = vec![
            tune(synthetic_run("a", "v1"), |r| {
                r.cost_micros = 18;
                r.latency_ms = 100;
            }),
            tune(synthetic_run("b", "v1"), |r| {
                r.steps = 2;
                r.attempts = 2;
                r.latency_ms = 400;
                r.intervention_points = 1;
                r.decision_points = 1;
                r.recovery = Some(synthetic_verdict(true, 2));
            }),
        ];
        let v2_runs = vec![
            tune(synthetic_run("a", "v2"), |r| {
                r.steps = 2;
                r.attempts = 2;
                r.cost_micros = 18;
                r.latency_ms = 150;
            }),
            tune(synthetic_run("b", "v2"), |r| {
                r.completed = false;
                r.latency_ms = 500;
                r.intervention_points = 1;
                r.decision_points = 1;
                r.recovery = Some(synthetic_verdict(false, 1));
                r.expectation_met = false;
            }),
        ];
        let s1 = summarize("v1", &v1_runs);
        let s2 = summarize("v2", &v2_runs);
        let c = compare(&s1, &s2, &v1_runs, &v2_runs);
        assert_eq!(c.baseline, "v1");
        assert_eq!(c.candidate, "v2");
        assert!((c.success_rate_delta - (-0.5)).abs() < 1e-9);
        assert_eq!(c.recovery_rate_delta, Some(-1.0));
        assert_eq!(c.cost_delta_micros, 0);
        assert_eq!(c.steps_delta, 0);
        assert_eq!(c.attempts_delta, 0);
        assert_eq!(c.mean_latency_delta_ms, 75);
        assert_eq!(c.per_case.len(), 2);
        let b = c.per_case.iter().find(|d| d.case == "b").unwrap();
        assert!(b.completed_v1 && !b.completed_v2);
        assert_eq!(b.recovered_v1, Some(true));
        assert_eq!(b.recovered_v2, Some(false));
        assert_eq!(b.attempts_v1, 2);
        assert_eq!(b.attempts_v2, 1);
    }

    // ---- the recovery rules (deterministic) ---------------------------------

    #[test]
    fn recovery_verdict_rules() {
        // Transient: completed + exactly 2 attempts (1 failed) + no DLQ.
        assert!(recovery_verdict(Injection::TransientFailOnce, true, 2, 1, true, false).correct);
        assert!(!recovery_verdict(Injection::TransientFailOnce, true, 3, 1, true, false).correct);
        assert!(!recovery_verdict(Injection::TransientFailOnce, false, 1, 1, false, true).correct);
        // Crash: completed + 2 attempts + ZERO fail events (expiry, not
        // failure) + no DLQ.
        assert!(recovery_verdict(Injection::CrashAndRedeliver, true, 2, 0, true, false).correct);
        assert!(!recovery_verdict(Injection::CrashAndRedeliver, true, 2, 1, true, false).correct);
        // Permanent: blocked + exactly 1 attempt + DLQ (no retry storm).
        assert!(recovery_verdict(Injection::PermanentFail, false, 1, 1, false, true).correct);
        assert!(!recovery_verdict(Injection::PermanentFail, false, 3, 3, false, true).correct);
        assert!(!recovery_verdict(Injection::PermanentFail, true, 1, 1, true, false).correct);
    }

    // ---- the report model ----------------------------------------------------

    fn report_fixture() -> RegressionReport {
        let v1_runs = vec![
            tune(synthetic_run("a", "v1"), |r| {
                r.cost_micros = 18;
                r.latency_ms = 100;
            }),
            tune(synthetic_run("b", "v1"), |r| {
                r.steps = 2;
                r.attempts = 2;
                r.latency_ms = 400;
                r.intervention_points = 1;
                r.decision_points = 1;
                r.recovery = Some(synthetic_verdict(true, 2));
            }),
        ];
        let v2_runs = vec![
            tune(synthetic_run("a", "v2"), |r| {
                r.steps = 2;
                r.attempts = 2;
                r.cost_micros = 18;
                r.latency_ms = 150;
            }),
            tune(synthetic_run("b", "v2"), |r| {
                r.completed = false;
                r.latency_ms = 500;
                r.intervention_points = 1;
                r.decision_points = 1;
                r.recovery = Some(synthetic_verdict(false, 1));
                r.expectation_met = false;
            }),
        ];
        let s1 = summarize("v1", &v1_runs);
        let s2 = summarize("v2", &v2_runs);
        RegressionReport {
            harness: "test harness".to_string(),
            cases: 2,
            versions: vec!["v1".to_string(), "v2".to_string()],
            per_case: [v1_runs.clone(), v2_runs.clone()].concat(),
            summary: vec![s1.clone(), s2.clone()],
            comparison: compare(&s1, &s2, &v1_runs, &v2_runs),
        }
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = report_fixture();
        let json = serde_json::to_string(&report).expect("serializes");
        let back: RegressionReport = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, report);
    }

    #[test]
    fn markdown_summary_mentions_both_versions_and_the_split() {
        let markdown = report_fixture().markdown_summary();
        assert!(markdown.contains("## Per-version metrics"));
        assert!(markdown.contains("| v1 |"));
        assert!(markdown.contains("| v2 |"));
        assert!(markdown.contains("## v1 → v2 deltas"));
        assert!(markdown.contains("reported only"));
        assert!(markdown.contains("Per-case × version"));
        assert!(markdown.contains("| a | v1 |"));
        assert!(markdown.contains("| b | v2 |"));
    }

    // ---- intervention counting -----------------------------------------------

    #[test]
    fn intervention_counts_follow_the_flow_spec() {
        // Approval (approved): 1 user decision + 1 `__approval` input over
        // 1 user decision + 1 side-effect gate.
        let approved = crate::flows::approval_flow(true);
        assert_eq!(intervention_counts(&approved), (2, 2));
        // Approval (rejected): the human decided, but no `__approval`.
        let rejected = crate::flows::approval_flow(false);
        assert_eq!(intervention_counts(&rejected), (1, 2));
        // Tool (approved): the `__approval` input over the tool gate.
        let tool = tool_flow("m", true);
        assert_eq!(intervention_counts(&tool), (1, 1));
        // Retrieval: no human touchpoints at all.
        let retrieval = retrieval_flow();
        assert_eq!(intervention_counts(&retrieval), (0, 0));
    }

    // ---- the injection hook (worker-side, no server) -------------------------

    fn dead_ctx(task_id: &str) -> TaskContext {
        TaskContext::new(
            "w-eval-unit".to_string(),
            task_id.to_string(),
            EngineClient::new("http://127.0.0.1:1"),
        )
    }

    fn handler(injection: Injection, target: &str) -> InjectingHandler {
        let options = FlowOptions::default();
        let deps = Arc::new(HandlerDeps::new(
            Arc::new(MockModelProvider::new()),
            &options,
        ));
        let declared = "ref-retrieve";
        InjectingHandler::new(
            declared,
            ReferenceHandler::new(declared.to_string(), deps),
            Some(injection),
            Some(target.to_string()),
            Arc::new(InjectionState::default()),
        )
    }

    fn retrieve_task(task_id: &str) -> ExternalTask {
        ExternalTask {
            task_id: task_id.to_string(),
            task_type: "ref-retrieve".to_string(),
            variables: HashMap::from([("topic".to_string(), "Q3".to_string())]),
            lock_expire_at: None,
            retries: 3,
            #[cfg(debug_assertions)]
            token_id: None,
            #[cfg(debug_assertions)]
            process_instance_id: None,
        }
    }

    #[tokio::test]
    async fn transient_injection_fails_once_then_delegates() {
        let handler = handler(Injection::TransientFailOnce, "ref-retrieve");
        let task = retrieve_task("task-t1");
        match handler.handle(task.clone(), dead_ctx("task-t1")).await {
            TaskResult::Fail {
                error, category, ..
            } => {
                assert!(error.starts_with(INJECTED_TRANSIENT_FAILURE), "{error}");
                assert_eq!(category, Some(RetryCategory::Transient));
            }
            TaskResult::Complete { .. } => panic!("the first attempt must fail"),
        }
        // The redelivered attempt (same task id) runs the real handler.
        match handler.handle(task, dead_ctx("task-t1")).await {
            TaskResult::Complete { variables } => {
                assert_eq!(
                    variables.get("documents").map(String::as_str),
                    Some(crate::handler::REFERENCE_DOCUMENTS)
                );
            }
            TaskResult::Fail { .. } => panic!("the redelivered attempt must succeed"),
        }
    }

    #[tokio::test]
    async fn permanent_injection_fails_every_attempt() {
        let handler = handler(Injection::PermanentFail, "ref-retrieve");
        for _ in 0..2 {
            match handler
                .handle(retrieve_task("task-p1"), dead_ctx("task-p1"))
                .await
            {
                TaskResult::Fail {
                    error, category, ..
                } => {
                    assert!(error.starts_with(INJECTED_PERMANENT_FAILURE), "{error}");
                    assert_eq!(category, Some(RetryCategory::Permanent));
                }
                TaskResult::Complete { .. } => panic!("permanent injection must fail"),
            }
        }
    }

    #[tokio::test]
    async fn non_target_task_types_delegate_untouched() {
        let handler = handler(Injection::PermanentFail, "ref-run-tool");
        let task = ExternalTask {
            task_id: "task-x".to_string(),
            task_type: "ref-retrieve".to_string(),
            variables: HashMap::new(),
            lock_expire_at: None,
            retries: 3,
            #[cfg(debug_assertions)]
            token_id: None,
            #[cfg(debug_assertions)]
            process_instance_id: None,
        };
        match handler.handle(task, dead_ctx("task-x")).await {
            TaskResult::Complete { .. } => {}
            TaskResult::Fail { error, .. } => panic!("must delegate, got: {error}"),
        }
    }
}
