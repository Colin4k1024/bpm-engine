//! Integration tests for the five AI reference flows (E3-4, issue #18).
//!
//! REST-level verification over real HTTP (ephemeral port, memory storage)
//! with the REAL reference worker and the offline [`MockModelProvider`] —
//! no network beyond `127.0.0.1`, no API keys, no clock-dependent
//! assertions. These run in default CI via `cargo test --workspace`.
//!
//! Covered per flow:
//!
//! - **检索 (retrieval)**: `retrieve → summarize (model) → end`; the
//!   summarize step's `/steps` envelope summary carries the model ref and
//!   the price-table cost; the chained task consumes the plain task's
//!   output.
//! - **推理 (inference)**: the model's `verdict` routes the exclusive
//!   gateway — low risk → approved branch / end, high risk → rejected
//!   branch / end.
//! - **审批 (approval)**: the human decision travels through the user
//!   task's variables; WITHOUT `__approval` the `act` side-effect tool is
//!   blocked (`tool_requires_approval`, permanent → Failed + DLQ, instance
//!   stays RUNNING); WITH it the tool executes.
//! - **工具 (tool)**: the E3-3 boundary end to end — approval gate,
//!   env-only credential (missing → permanent `credential_missing`),
//!   credential masking of the tool output, envelope recording the tool.
//! - **补偿 (compensation)**: the failure path routes to the compensating
//!   step which undoes the forward steps in REVERSE order (saga shape);
//!   the happy path ships; and [`SagaCoordinator`] records/compensates
//!   against the LIVE server's compensation record store.

use std::sync::Arc;
use std::time::Duration;

use bpm_engine_ai_reference::{
    approval_flow, compensation_flow, deploy_flow, inference_flow, retrieval_flow, run_flow,
    run_flow_with, tool_flow, FlowOptions, MockModelProvider, ModelProvider, SagaCoordinator,
    SAGA_FORWARD_STEPS, STATUS_PENDING,
};
use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::{build_app, state::AppState};
use tokio_util::sync::CancellationToken;

struct TestServer {
    base_url: String,
    state: Arc<AppState>,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

/// Build the app and serve it on an ephemeral port (harness pattern from
/// `crates/server/rest/tests/worker_context.rs`).
async fn spawn_server() -> TestServer {
    let config = BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        rate_limit: RateLimitConfig {
            requests_per_minute: 1_000_000,
        },
        ..Default::default()
    };
    let app = build_app(&config).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app.router).await;
    });
    TestServer {
        base_url: format!("http://{addr}"),
        state: app.state,
        serve_task,
        timer_cancel: app.timer_cancel,
    }
}

impl TestServer {
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn mock_provider() -> Arc<dyn ModelProvider> {
    Arc::new(MockModelProvider::new())
}

/// Deploy a flow's DSL document through the engine's definition deployer.
async fn deploy(server: &TestServer, flow: &bpm_engine_ai_reference::FlowSpec) {
    deploy_flow(server.state.storage.deployer.as_ref(), flow)
        .await
        .expect("deploy flow");
}

// ---------------------------------------------------------------------------
// 检索 Retrieval: retrieve → summarize (model) → end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retrieval_flow_summarizes_documents_with_model_envelope() {
    let server = spawn_server().await;
    let flow = retrieval_flow();
    deploy(&server, &flow).await;

    let outcome = run_flow(&server.base_url, flow, mock_provider())
        .await
        .expect("run retrieval flow");

    // The instance completed through both tasks.
    assert!(outcome.is_completed(), "state: {}", outcome.state);

    // The plain task's output fed the model task (chained variables).
    assert_eq!(
        outcome.variable("documents"),
        Some("invoice-2026-Q3.pdf;receipt-2026-Q3.pdf")
    );
    assert_eq!(
        outcome.variable("summary"),
        Some(bpm_engine_ai_reference::MOCK_SUMMARY_TEXT)
    );

    // The final variables carry the E3-1 envelope: model ref + cost from
    // the price table ((48 * 250 + 12 * 500) / 1000 = 18 micro-USD).
    let envelope = outcome
        .envelope()
        .expect("envelope in final variables")
        .expect("valid envelope");
    assert_eq!(
        envelope.model.as_ref().map(|m| m.name.as_str()),
        Some("mock-model-v1")
    );
    assert_eq!(envelope.cost.as_ref().unwrap().amount_micros, Some(18));
    assert!(envelope.execution_id.ends_with(":0"));

    // The step timeline: retrieve ran BEFORE summarize, and the summarize
    // step's envelope summary surfaces model + cost (no variable values).
    let retrieve = outcome.step("retrieve").expect("retrieve step");
    let summarize = outcome.step("summarize").expect("summarize step");
    assert_eq!(summarize.kind, "external_task");
    assert!(retrieve.step_no < summarize.step_no);
    assert!(retrieve.executed());
    assert!(summarize.executed());
    let summary = summarize.envelope.as_ref().expect("step envelope summary");
    assert_eq!(summary["model"]["name"], "mock-model-v1");
    assert_eq!(summary["model"]["provider"], "mock");
    assert_eq!(summary["cost"]["amount_micros"], 18);
    assert_eq!(summary["cost"]["currency"], "USD");
    // Plain (non-AI) retrieve step carries no envelope.
    assert!(retrieve.envelope.is_none());

    server.stop().await;
}

// ---------------------------------------------------------------------------
// 推理 Inference: classify (model) → gateway → two ends
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inference_flow_routes_on_the_model_verdict() {
    let server = spawn_server().await;

    // Low risk: the mock classifies "approved" -> conditional branch.
    let flow = inference_flow("low");
    deploy(&server, &flow).await;
    let approved = run_flow(&server.base_url, flow, mock_provider())
        .await
        .expect("run inference (approved)");

    assert!(approved.is_completed(), "state: {}", approved.state);
    assert_eq!(approved.variable("verdict"), Some("approved"));
    assert_eq!(approved.variable("action"), Some("approved"));
    assert!(approved.has_step("act-approve"), "approved branch ran");
    assert!(!approved.has_step("act-reject"), "rejected branch skipped");
    assert!(approved.has_step("end-approved"));
    // Model envelope on the classify step: (24 * 250 + 2 * 500) / 1000 = 7.
    let classify = approved.step("classify").expect("classify step");
    let summary = classify.envelope.as_ref().expect("classify envelope");
    assert_eq!(summary["cost"]["amount_micros"], 7);

    // High risk: the mock classifies "rejected" -> default branch.
    let flow = inference_flow("high");
    deploy(&server, &flow).await;
    let rejected = run_flow(&server.base_url, flow, mock_provider())
        .await
        .expect("run inference (rejected)");

    assert!(rejected.is_completed(), "state: {}", rejected.state);
    assert_eq!(rejected.variable("verdict"), Some("rejected"));
    assert_eq!(rejected.variable("action"), Some("rejected"));
    assert!(rejected.has_step("act-reject"), "rejected branch ran");
    assert!(!rejected.has_step("act-approve"), "approved branch skipped");
    assert!(rejected.has_step("end-rejected"));

    server.stop().await;
}

// ---------------------------------------------------------------------------
// 审批 Approval: propose (model) → userTask → act (side-effect tool)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn approval_flow_without_approval_blocks_the_side_effect_tool() {
    let server = spawn_server().await;
    let flow = approval_flow(false); // human rejects: no __approval variable
    deploy(&server, &flow).await;

    // Unique env var per test (std::env is process-global, tests parallel):
    // present, so the asserted failure is the GATE, not the credential.
    std::env::set_var("BPM_TEST_AIREF_APPROVAL_LEDGER", "sk-approval-test");
    let options = FlowOptions::default()
        .timeout(Duration::from_secs(3)) // blocked flow: short wait
        .tool_env_var("BPM_TEST_AIREF_APPROVAL_LEDGER");

    let outcome = run_flow_with(&server.base_url, flow, mock_provider(), options)
        .await
        .expect("run approval flow (unapproved)");

    // Fail-closed: the act task failed permanently (no retries fix a policy
    // refusal) -> the instance stays RUNNING with the token at `act`.
    assert_eq!(outcome.state, "RUNNING");
    assert!(
        outcome.current_nodes.contains(&"act".to_string()),
        "token at act"
    );
    let act = outcome.step("act").expect("act step");
    assert!(act.failed(), "act step must be failed");
    // The E3-2 structured category prefixes the code in recorded errors.
    assert!(
        act.attempt_errors
            .iter()
            .any(|e| e.contains("tool_requires_approval")),
        "attempt errors: {:?}",
        act.attempt_errors
    );
    // The side effect never ran.
    assert!(outcome.variable("ledger_result").is_none());

    // The DLQ caught the permanently failed task (the engine's retry
    // exhaustion path — no redelivery).
    let dlq = server
        .state
        .storage
        .dead_letter_store
        .list(None, 100)
        .await
        .expect("dead letter list");
    assert!(
        dlq.iter()
            .any(|entry| entry.error_message.contains("tool_requires_approval")),
        "DLQ must carry the blocked side-effect task, got: {:?}",
        dlq.iter()
            .map(|e| e.error_message.clone())
            .collect::<Vec<_>>()
    );

    // The model task BEFORE the gate still ran with its envelope.
    let propose = outcome.step("propose").expect("propose step");
    assert!(propose.executed());
    assert!(propose.envelope.is_some());

    std::env::remove_var("BPM_TEST_AIREF_APPROVAL_LEDGER");
    server.stop().await;
}

#[tokio::test]
async fn approval_flow_with_user_approval_executes_the_side_effect_tool() {
    let server = spawn_server().await;
    let flow = approval_flow(true); // human approves: __approval = "true"
    deploy(&server, &flow).await;

    std::env::set_var("BPM_TEST_AIREF_APPROVAL2_LEDGER", "sk-approval2-test");
    let options = FlowOptions::default().tool_env_var("BPM_TEST_AIREF_APPROVAL2_LEDGER");

    let outcome = run_flow_with(&server.base_url, flow, mock_provider(), options)
        .await
        .expect("run approval flow (approved)");

    assert!(outcome.is_completed(), "state: {}", outcome.state);
    // The user task completed through the human decision path (its
    // completion moves the token; v1 does not merge its variables into the
    // instance — the approval rides in the start variables, see
    // approval_flow's docs).
    let approve = outcome.step("approve").expect("approve step");
    assert!(approve.executed());
    // The proposal came from the model (mock echo route) with an envelope.
    assert_eq!(
        outcome.variable("proposal"),
        Some("echo: propose approval for: quarterly ledger adjustment of 42 USD")
    );
    // The side-effect tool ran; its output is credential-masked.
    let ledger_result = outcome.variable("ledger_result").expect("tool output");
    assert_eq!(
        ledger_result,
        "ledger: posted 'quarterly ledger adjustment' with key=***"
    );
    // The tool is recorded in the act step's envelope.
    let act = outcome.step("act").expect("act step");
    let summary = act.envelope.as_ref().expect("act envelope");
    assert_eq!(summary["tools"][0]["name"], "ledger_post");

    std::env::remove_var("BPM_TEST_AIREF_APPROVAL2_LEDGER");
    server.stop().await;
}

// ---------------------------------------------------------------------------
// 工具 Tool: run-tool (side-effect tool through the E3-3 boundary)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_flow_without_approval_is_blocked() {
    let server = spawn_server().await;
    let flow = tool_flow("memo-no-approval", false);
    deploy(&server, &flow).await;

    std::env::set_var("BPM_TEST_AIREF_TOOL_LEDGER", "sk-tool-test");
    let options = FlowOptions::default()
        .timeout(Duration::from_secs(3))
        .tool_env_var("BPM_TEST_AIREF_TOOL_LEDGER");

    let outcome = run_flow_with(&server.base_url, flow, mock_provider(), options)
        .await
        .expect("run tool flow (unapproved)");

    assert_eq!(outcome.state, "RUNNING");
    let run_tool = outcome.step("run-tool").expect("run-tool step");
    assert!(run_tool.failed());
    assert!(
        run_tool
            .attempt_errors
            .iter()
            .any(|e| e.contains("tool_requires_approval")),
        "attempt errors: {:?}",
        run_tool.attempt_errors
    );
    assert!(outcome.variable("ledger_result").is_none());

    std::env::remove_var("BPM_TEST_AIREF_TOOL_LEDGER");
    server.stop().await;
}

#[tokio::test]
async fn tool_flow_with_approval_runs_and_masks_the_credential() {
    let server = spawn_server().await;
    let flow = tool_flow("quarterly close entry", true); // __approval start var
    deploy(&server, &flow).await;

    std::env::set_var("BPM_TEST_AIREF_TOOL2_LEDGER", "sk-tool2-secret");
    let options = FlowOptions::default().tool_env_var("BPM_TEST_AIREF_TOOL2_LEDGER");

    let outcome = run_flow_with(&server.base_url, flow, mock_provider(), options)
        .await
        .expect("run tool flow (approved)");

    assert!(outcome.is_completed(), "state: {}", outcome.state);
    let ledger_result = outcome.variable("ledger_result").expect("tool output");
    // Masked: the raw tool output embeds the credential; the E3-3 pipeline
    // replaces it before anything is persisted.
    assert_eq!(
        ledger_result,
        "ledger: posted 'quarterly close entry' with key=***"
    );
    // No variable anywhere leaks the raw credential.
    assert!(
        !outcome
            .variables
            .values()
            .any(|v| v.contains("sk-tool2-secret")),
        "credential leaked into variables: {:?}",
        outcome.variables.keys()
    );
    // The envelope records the executed tool.
    let run_tool = outcome.step("run-tool").expect("run-tool step");
    let summary = run_tool.envelope.as_ref().expect("run-tool envelope");
    assert_eq!(summary["tools"][0]["name"], "ledger_post");
    assert_eq!(summary["tools"][0]["version"], "1.0");

    std::env::remove_var("BPM_TEST_AIREF_TOOL2_LEDGER");
    server.stop().await;
}

#[tokio::test]
async fn tool_flow_with_missing_credential_fails_permanently() {
    let server = spawn_server().await;
    let flow = tool_flow("entry-without-credential", true);
    deploy(&server, &flow).await;

    // Never set: the injector cannot resolve it (permanent failure naming
    // the env var, never a value).
    std::env::remove_var("BPM_TEST_AIREF_TOOL3_LEDGER");
    let options = FlowOptions::default()
        .timeout(Duration::from_secs(3))
        .tool_env_var("BPM_TEST_AIREF_TOOL3_LEDGER");

    let outcome = run_flow_with(&server.base_url, flow, mock_provider(), options)
        .await
        .expect("run tool flow (missing credential)");

    assert_eq!(outcome.state, "RUNNING");
    let run_tool = outcome.step("run-tool").expect("run-tool step");
    assert!(
        run_tool
            .attempt_errors
            .iter()
            .any(|e| e.contains("credential_missing") && e.contains("BPM_TEST_AIREF_TOOL3_LEDGER")),
        "attempt errors: {:?}",
        run_tool.attempt_errors
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// 补偿 Compensation: reserve → charge → gateway → undo (reverse) / ship
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compensation_flow_failure_path_compensates_in_reverse_order() {
    let server = spawn_server().await;
    let flow = compensation_flow(true); // charge reports failure
    deploy(&server, &flow).await;

    let outcome = run_flow(&server.base_url, flow, mock_provider())
        .await
        .expect("run compensation flow (failed)");

    // The saga compensated and reached the compensated end.
    assert!(outcome.is_completed(), "state: {}", outcome.state);
    assert_eq!(outcome.variable("saga_outcome"), Some("failed"));
    assert!(outcome.has_step("undo"));
    assert!(outcome.has_step("end-compensated"));
    assert!(!outcome.has_step("ship"));

    // REVERSE order: charge (completed last) is undone FIRST.
    assert_eq!(
        outcome.variable("compensation_log"),
        Some("undo-charge;undo-reserve")
    );
    // The forward steps completed before the compensation ran.
    assert_eq!(outcome.variable("reserved"), Some("true"));
    assert_eq!(outcome.variable("charged"), Some("true"));
    assert!(outcome.variable("shipped").is_none());

    server.stop().await;
}

#[tokio::test]
async fn compensation_flow_happy_path_ships_without_compensation() {
    let server = spawn_server().await;
    let flow = compensation_flow(false);
    deploy(&server, &flow).await;

    let outcome = run_flow(&server.base_url, flow, mock_provider())
        .await
        .expect("run compensation flow (ok)");

    assert!(outcome.is_completed(), "state: {}", outcome.state);
    assert_eq!(outcome.variable("saga_outcome"), Some("ok"));
    assert_eq!(outcome.variable("shipped"), Some("true"));
    assert!(outcome.has_step("ship"));
    assert!(outcome.has_step("end-ok"));
    // No compensation on the happy path.
    assert!(!outcome.has_step("undo"));
    assert!(outcome.variable("compensation_log").is_none());

    server.stop().await;
}

/// The engine-side saga machinery: forward completions recorded into the
/// LIVE server's compensation store, then compensated newest-first through
/// [`SagaCoordinator`] — the application-coordinated loop the engine's
/// design assigns to embedders (`docs/architecture.md`, `core/src/saga.rs`).
#[tokio::test]
async fn saga_coordinator_records_and_compensates_over_the_engine_store() {
    let server = spawn_server().await;
    let flow = compensation_flow(true);
    deploy(&server, &flow).await;
    let outcome = run_flow(&server.base_url, flow, mock_provider())
        .await
        .expect("run compensation flow");
    assert!(outcome.is_completed());

    let repo = Arc::clone(&server.state.storage.compensation_repo);

    // Record the forward steps exactly as an application coordinator does:
    // observing the step trace (reserve ran before charge).
    SagaCoordinator::record_from_steps(repo.as_ref(), &outcome, &SAGA_FORWARD_STEPS)
        .await
        .expect("record forward steps");

    // The records live in the engine's store: Pending, sorted ascending.
    let rows = repo.list_by_instance(&outcome.instance_id).await;
    assert_eq!(rows.len(), 2, "both forward steps recorded");
    assert_eq!(rows[0].node_id, "reserve");
    assert_eq!(rows[0].order, 1);
    assert_eq!(rows[1].node_id, "charge");
    assert_eq!(rows[1].order, 2);
    assert!(rows.iter().all(|r| r.status == STATUS_PENDING));

    // Compensate newest-first; the undo observes the reverse order.
    let mut undone: Vec<String> = Vec::new();
    let outcomes = SagaCoordinator::compensate(repo.as_ref(), &outcome.instance_id, |record| {
        undone.push(record.node_id.clone());
        Ok(())
    })
    .await
    .expect("compensate");
    assert_eq!(undone, ["charge", "reserve"], "compensate newest first");
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| matches!(
        o,
        bpm_engine_ai_reference::CompensationOutcome::Completed { .. }
    )));

    server.stop().await;
}
