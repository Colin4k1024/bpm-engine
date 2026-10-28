//! The five reference flows (E3-4): DSL documents stored inline plus
//! [`FlowSpec`] constructors and compile/deploy helpers.
//!
//! Every flow is a plain JSON document in the engine's DSL v1
//! (`docs/dsl-guide.md`), stored inline as a `pub const` (`[RETRIEVAL_DSL]`
//! … [`COMPENSATION_DSL`]) so applications can embed, diff and validate
//! them. Constructors wrap each document (plus the task types its worker
//! must subscribe to, start variables and user-task decisions) into a
//! [`FlowSpec`]:
//!
//! | flow | shape | demonstrates |
//! |---|---|---|
//! | [`retrieval_flow`] (检索) | `retrieve` → `summarize` (model) → end | a model task consuming a plain task's output; envelope with model + cost in the step trace |
//! | [`inference_flow`] (推理) | `classify` (model) → exclusive gateway → two ends | the model's verdict routing the flow via `variableEq` |
//! | [`approval_flow`] (审批) | `propose` (model) → `userTask` → `act` (tool) | human approval through task variables (`__approval`), fail-closed side-effect gate |
//! | [`tool_flow`] (工具) | `run-tool` (tool) → end | E3-3 tool boundary: allowlist, approval gate, env-only credential, output masking |
//! | [`compensation_flow`] (补偿) | `reserve` → `charge` → gateway → `undo` or `ship` | saga shape: the failure path compensates completed steps in reverse order |
//!
//! Deploying: the REST `POST /process-definitions/deploy` endpoint accepts
//! BPMN XML only, so DSL flows are registered against the definition store
//! directly ([`deploy_flow`] — the same in-process bootstrap pattern as
//! `Storage::memory_with_def_store`). Both the integration tests and
//! `examples/ai_flow.rs` do exactly this before calling
//! [`run_flow`](crate::run_flow).

use std::collections::HashMap;

use anyhow::anyhow;

use crate::handler::task_type;
use bpm_engine_core::ProcessDefinition;
use bpm_engine_storage::DefinitionDeployer;

/// Retrieval flow (检索): `start → retrieve → summarize (model) → end`.
///
/// The `retrieve` external task returns a fixed document list; `summarize`
/// feeds it to the model provider and completes with the E3-1 envelope
/// (model ref, cost, execution id) riding in its variables.
pub const RETRIEVAL_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-retrieval", "name": "AI Reference: Retrieval" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "retrieve", "type": "serviceTask", "task_type": "ref-retrieve", "name": "Retrieve documents" },
    { "id": "summarize", "type": "serviceTask", "task_type": "ref-summarize", "name": "Summarize with model" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "retrieve" },
    { "source": "retrieve", "target": "summarize" },
    { "source": "summarize", "target": "end" }
  ]
}"#;

/// Inference flow (推理): `classify` (model) → exclusive gateway on the
/// model's `verdict` variable → `act-approve` / `act-reject` → two ends.
pub const INFERENCE_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-inference", "name": "AI Reference: Inference" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "classify", "type": "serviceTask", "task_type": "ref-classify", "name": "Classify with model" },
    { "id": "decide", "type": "exclusiveGateway", "name": "Route on verdict" },
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
      "target": "act-approve",
      "condition": { "kind": "variableEq", "variable": "verdict", "value": "approved" }
    },
    { "source": "decide", "target": "act-reject", "default": true },
    { "source": "act-approve", "target": "end-approved" },
    { "source": "act-reject", "target": "end-rejected" }
  ]
}"#;

/// Approval flow (审批): `propose` (model) → `approve` (user task with an
/// enum form) → `act` (side-effect tool behind the E3-3 approval gate) →
/// end.
///
/// The human's decision is submitted through the user task; the
/// `__approval` task input variable is what unlocks the `act` tool
/// (fail-closed: no approval → `tool_requires_approval`, straight to
/// Failed + DLQ). See [`approval_flow`] for how the two travel in v1.
pub const APPROVAL_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-approval", "name": "AI Reference: Approval" },
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
    { "id": "act", "type": "serviceTask", "task_type": "ref-act", "name": "Execute approved action" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "propose" },
    { "source": "propose", "target": "approve" },
    { "source": "approve", "target": "act" },
    { "source": "act", "target": "end" }
  ]
}"#;

/// Tool flow (工具): `start → run-tool (side-effect tool) → end`.
///
/// The worker executes the `ledger_post` tool through the E3-3 boundary:
/// allowlist (deny by default), approval gate (`__approval` task input),
/// credentials from the worker environment only, and credential-masked,
/// size-limited output variables.
pub const TOOL_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-tool", "name": "AI Reference: Tool" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "run-tool", "type": "serviceTask", "task_type": "ref-run-tool", "name": "Post to ledger via tool" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "run-tool" },
    { "source": "run-tool", "target": "end" }
  ]
}"#;

/// Compensation flow (补偿): `reserve → charge → gateway`: on
/// `saga_outcome = failed` run `undo` (compensates completed steps in
/// reverse order) and end compensated; otherwise `ship` and end ok.
///
/// This models the engine's saga shape (`docs/architecture.md` §Failure &
/// Compensation, `crates/core/src/saga.rs`): forward steps complete, a
/// later failure triggers compensation of the completed work in REVERSE
/// order. The engine persists compensation records
/// ([`CompensationRecordRepo`](bpm_engine_storage::CompensationRecordRepo))
/// but the loop itself is application-coordinated — see
/// [`SagaCoordinator`](crate::saga::SagaCoordinator) for the record-backed
/// loop and the integration test wiring it against a live server's store.
pub const COMPENSATION_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "ai-reference-compensation", "name": "AI Reference: Saga Compensation" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "reserve", "type": "serviceTask", "task_type": "ref-saga-reserve", "name": "Reserve stock" },
    { "id": "charge", "type": "serviceTask", "task_type": "ref-saga-charge", "name": "Charge payment" },
    { "id": "saga-gate", "type": "exclusiveGateway", "name": "Saga outcome" },
    { "id": "undo", "type": "serviceTask", "task_type": "ref-saga-compensate", "name": "Compensate (reverse order)" },
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
    { "source": "undo", "target": "end-compensated" },
    { "source": "ship", "target": "end-ok" }
  ]
}"#;

/// One user-task decision the flow runner submits on the flow's behalf:
/// when the instance's token reaches `node_id`, the runner completes that
/// user task with `variables` (e.g. the approval decision plus `__approval`
/// for side-effect tools).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTaskDecision {
    /// The user-task node id the decision applies to.
    pub node_id: String,
    /// Variables submitted with the completion (plain strings).
    pub variables: HashMap<String, String>,
}

/// A runnable reference flow: the DSL document plus everything
/// [`run_flow`](crate::run_flow) needs to drive it (task types to subscribe
/// to, start variables, user-task decisions to submit).
#[derive(Debug, Clone)]
pub struct FlowSpec {
    /// Short display name ("retrieval", "approval", ...).
    pub name: String,
    /// The DSL process id (used as the process definition id on deploy and
    /// to start instances).
    pub process_def_id: String,
    /// The full DSL JSON document.
    pub dsl_json: String,
    /// External-task types the reference worker subscribes to for this flow.
    pub task_types: Vec<String>,
    /// Variables submitted when starting the instance.
    pub start_variables: HashMap<String, String>,
    /// User-task decisions submitted while the instance runs.
    pub user_decisions: Vec<UserTaskDecision>,
}

impl FlowSpec {
    /// A flow spec from its parts (no start variables, no user decisions).
    pub fn new(
        name: impl Into<String>,
        process_def_id: impl Into<String>,
        dsl_json: impl Into<String>,
        task_types: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            process_def_id: process_def_id.into(),
            dsl_json: dsl_json.into(),
            task_types: task_types.into_iter().map(Into::into).collect(),
            start_variables: HashMap::new(),
            user_decisions: Vec::new(),
        }
    }

    /// Builder: set/override the start variables.
    pub fn with_variables(
        mut self,
        variables: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        self.start_variables = variables
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self
    }

    /// Builder: add one user-task decision.
    pub fn with_user_decision(
        mut self,
        node_id: impl Into<String>,
        variables: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        self.user_decisions.push(UserTaskDecision {
            node_id: node_id.into(),
            variables: variables
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        });
        self
    }

    /// The DSL document (alias for `&self.dsl_json`).
    pub fn dsl(&self) -> &str {
        &self.dsl_json
    }
}

/// Retrieval flow (检索) with a sample topic.
///
/// Start variable `topic` labels the retrieval; the `summarize` model task
/// runs on whatever `documents` the `retrieve` task returned.
pub fn retrieval_flow() -> FlowSpec {
    FlowSpec::new(
        "retrieval",
        "ai-reference-retrieval",
        RETRIEVAL_DSL,
        [task_type::RETRIEVE, task_type::SUMMARIZE],
    )
    .with_variables([("topic", "Q3 revenue close")])
}

/// Inference flow (推理) with the model's deciding variable.
///
/// `risk` reaches the model task's input variables; the mock provider
/// classifies `"high"` risk as rejected and everything else as approved,
/// and the gateway routes on the resulting `verdict`.
pub fn inference_flow(risk: &str) -> FlowSpec {
    FlowSpec::new(
        "inference",
        "ai-reference-inference",
        INFERENCE_DSL,
        [
            task_type::CLASSIFY,
            task_type::ACT_APPROVE,
            task_type::ACT_REJECT,
        ],
    )
    .with_variables([("risk", risk), ("subject", "expense refund request #4482")])
}

/// Approval flow (审批).
///
/// `approve = true`: the human submits `decision = "approved"` at the
/// `approve` user task AND the instance carries `__approval = "true"` —
/// the E3-3 task-input approval contract — so the `act` side-effect tool
/// executes. `approve = false`: the human rejects, no `__approval` — the
/// tool is then blocked (`tool_requires_approval`, permanent failure).
///
/// v1 honesty: user-task completion variables are not merged into the
/// instance by the runtime today (external-task completions are), so the
/// approver's `__approval` rides in the instance variables — the task
/// variables channel the SDK's gate actually reads. The user task records
/// the human decision in the flow; the engine-side persistent approval
/// flow (E5-2) will set `__approval` automatically in a future release.
pub fn approval_flow(approve: bool) -> FlowSpec {
    let mut flow = FlowSpec::new(
        "approval",
        "ai-reference-approval",
        APPROVAL_DSL,
        [task_type::PROPOSE, task_type::ACT],
    )
    .with_variables([
        ("subject", "quarterly ledger adjustment of 42 USD"),
        ("memo", "quarterly ledger adjustment"),
    ]);
    if approve {
        // The approval variable path (E3-3 v1): plain "true" in the
        // instance variables reaches the act task's input snapshot.
        flow.start_variables
            .insert("__approval".to_string(), "true".to_string());
    }
    flow = flow.with_user_decision(
        "approve",
        [("decision", if approve { "approved" } else { "rejected" })],
    );
    flow
}

/// Tool flow (工具).
///
/// `memo` is the ledger entry the tool posts; `approved` decides whether
/// the start variables carry `__approval = "true"` (the variable path the
/// E3-3 gate reads — an alternative to routing approval through a user
/// task). The credential comes from the worker environment
/// ([`FlowOptions::tool_env_var`](crate::FlowOptions::tool_env_var)).
pub fn tool_flow(memo: &str, approved: bool) -> FlowSpec {
    let mut flow = FlowSpec::new("tool", "ai-reference-tool", TOOL_DSL, [task_type::RUN_TOOL])
        .with_variables([("memo", memo)]);
    if approved {
        flow.start_variables
            .insert("__approval".to_string(), "true".to_string());
    }
    flow
}

/// Compensation flow (补偿).
///
/// `fail = true` makes the `charge` step report `saga_outcome = failed`;
/// the gateway then routes to `undo`, which compensates the completed
/// forward steps in reverse order. `fail = false` ships the order.
pub fn compensation_flow(fail: bool) -> FlowSpec {
    FlowSpec::new(
        "compensation",
        "ai-reference-compensation",
        COMPENSATION_DSL,
        [
            task_type::SAGA_RESERVE,
            task_type::SAGA_CHARGE,
            task_type::SAGA_COMPENSATE,
            task_type::SAGA_SHIP,
        ],
    )
    .with_variables([
        ("order_id", "order-2026-4482"),
        ("saga_fail", if fail { "true" } else { "false" }),
    ])
}

/// Compile a flow's DSL document into a [`ProcessDefinition`]
/// (validation included — a broken flow document fails here, before any
/// engine state exists).
pub fn compile_flow(flow: &FlowSpec) -> anyhow::Result<ProcessDefinition> {
    bpm_engine_dsl::compile(&flow.dsl_json)
        .map_err(|report| anyhow!("flow '{}' failed to compile: {report:?}", flow.name))
}

/// Deploy a flow's DSL document to a definition store.
///
/// The REST deploy endpoint accepts BPMN XML only, so DSL definitions are
/// registered in-process through the [`DefinitionDeployer`] trait (the same
/// bootstrap pattern as `Storage::memory_with_def_store`). The DSL source
/// string is passed along verbatim; backends that persist sources keep it.
///
/// Call this once per flow before [`run_flow`](crate::run_flow):
///
/// ```no_run
/// # async fn example(
/// #     state: &bpm_engine_server_rest::state::AppState,
/// # ) -> anyhow::Result<()> {
/// use bpm_engine_ai_reference::{deploy_flow, retrieval_flow};
///
/// // `state.storage.deployer` is the engine's definition deployer.
/// deploy_flow(state.storage.deployer.as_ref(), &retrieval_flow()).await?;
/// # Ok(())
/// # }
/// ```
pub async fn deploy_flow(deployer: &dyn DefinitionDeployer, flow: &FlowSpec) -> anyhow::Result<()> {
    let definition = compile_flow(flow)?;
    deployer
        .deploy(&definition, &flow.dsl_json)
        .await
        .map_err(|e| anyhow!("flow '{}' failed to deploy: {e}", flow.name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipped DSL document must compile — the cheapest possible gate
    /// against a broken flow (no server, no worker).
    #[test]
    fn every_reference_dsl_document_compiles() {
        for (name, dsl) in [
            ("retrieval", RETRIEVAL_DSL),
            ("inference", INFERENCE_DSL),
            ("approval", APPROVAL_DSL),
            ("tool", TOOL_DSL),
            ("compensation", COMPENSATION_DSL),
        ] {
            let def = bpm_engine_dsl::compile(dsl)
                .unwrap_or_else(|report| panic!("flow '{name}' must compile: {report:?}"));
            assert!(!def.nodes.is_empty(), "flow '{name}' has no nodes");
        }
    }

    #[test]
    fn flow_specs_carry_their_task_types_and_defaults() {
        let retrieval = retrieval_flow();
        assert_eq!(retrieval.process_def_id, "ai-reference-retrieval");
        assert_eq!(retrieval.name, "retrieval");
        assert_eq!(retrieval.task_types, vec!["ref-retrieve", "ref-summarize"]);
        assert_eq!(
            retrieval.start_variables.get("topic").map(String::as_str),
            Some("Q3 revenue close")
        );

        let inference = inference_flow("high");
        assert_eq!(
            inference.start_variables.get("risk").map(String::as_str),
            Some("high")
        );
        assert_eq!(inference.task_types.len(), 3);

        // Approval: __approval rides in the START variables (the v1 task
        // variable channel the gate reads); the human decision is recorded
        // at the user task either way.
        let approved = approval_flow(true);
        assert_eq!(
            approved
                .start_variables
                .get("__approval")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            approved.user_decisions[0]
                .variables
                .get("decision")
                .map(String::as_str),
            Some("approved")
        );
        let rejected = approval_flow(false);
        assert!(!rejected.start_variables.contains_key("__approval"));
        assert_eq!(
            rejected.user_decisions[0]
                .variables
                .get("decision")
                .map(String::as_str),
            Some("rejected")
        );

        // Tool flow: __approval start variable only when approved.
        let tool = tool_flow("memo-1", true);
        assert_eq!(
            tool.start_variables.get("__approval").map(String::as_str),
            Some("true")
        );
        let tool = tool_flow("memo-1", false);
        assert!(!tool.start_variables.contains_key("__approval"));

        // Compensation: saga_fail flag.
        assert_eq!(
            compensation_flow(true)
                .start_variables
                .get("saga_fail")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            compensation_flow(false)
                .start_variables
                .get("saga_fail")
                .map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn compile_flow_reports_flow_name_on_error() {
        let broken = FlowSpec::new("broken", "broken-flow", "{ not json }", ["x"]);
        let err = compile_flow(&broken).expect_err("broken DSL must fail");
        assert!(err.to_string().contains("broken"), "{err}");
    }

    #[test]
    fn dsl_accessor_returns_the_document() {
        let flow = retrieval_flow();
        assert_eq!(flow.dsl(), RETRIEVAL_DSL);
    }
}
