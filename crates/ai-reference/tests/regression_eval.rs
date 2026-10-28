//! Deterministic regression-eval invariants (E6-3, issue #28).
//!
//! **The separation principle — 统计指标与确定性内核测试分开.** This file
//! is the DETERMINISTIC half of the E6-3 acceptance: a small, fast subset
//! of the harness's fixed case set (three cases × both definition
//! versions, plus the crash-redelivery case), asserting EXACT behavior —
//! completion states, attempt counts, costs per the mock provider's price
//! table, recovery-correctness verdicts, intervention counts and the
//! report's structural validity (both versions present, fields
//! non-negative, JSON round-trip, delta fields exist).
//!
//! The STATISTICAL/comparative output (success-rate deltas, latency
//! distributions, cost totals per version over the FULL case set) is
//! produced by the runnable harness
//! `cargo run -p bpm-engine-ai-reference --example regression_eval` and is
//! REPORTED ONLY — no assertion here is a wall-clock PERFORMANCE gate (the single <5000 ms bound at the bottom of the suite is a liveness/hang guard, not a metric) or an
//! aggregate rate's specific value beyond what determinism guarantees.
//!
//! Everything runs offline: real REST server on an ephemeral loopback port
//! (memory storage), real SDK workers, the deterministic mock provider,
//! short eval timings (2 s lease — the store's second-granular lease
//! expiry makes a 1 s lease a 0–1000 ms true lease — and a 200 ms worker
//! poll).

use std::sync::Arc;
use std::time::Duration;

use bpm_engine_ai_reference::eval::{
    fixed_case_set, fixture_version_selector, run_regression_with, CaseRun, RegressionReport,
    Version,
};
use bpm_engine_ai_reference::{FlowOptions, MockModelProvider, ModelProvider};
use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::{build_app, state::AppState};
use tokio_util::sync::CancellationToken;

/// The tool credential env var base (std::env is process-global and tests
/// run in PARALLEL: each test uses its own suffixed name, exactly like
/// `tests/reference_flows.rs`).
const EVAL_LEDGER_ENV_BASE: &str = "BPM_TEST_AIREF_EVAL_LEDGER";

struct TestServer {
    base_url: String,
    state: Arc<AppState>,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

/// Build the app and serve it on an ephemeral port (the harness pattern of
/// `tests/reference_flows.rs`).
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

/// The eval options for these tests: the short eval timings, a tight run
/// timeout (every case either terminates or dead-letters quickly) and the
/// given credential env var.
fn eval_options(ledger_env: &str) -> FlowOptions {
    let mut options = bpm_engine_ai_reference::eval::eval_flow_options();
    options.timeout = Duration::from_secs(5);
    options.tool_env_var = ledger_env.to_string();
    options
}

fn run_of<'a>(report: &'a RegressionReport, case: &str, version: Version) -> &'a CaseRun {
    report
        .per_case
        .iter()
        .find(|r| r.case == case && r.version == version.label())
        .unwrap_or_else(|| panic!("missing run of '{case}' ({:?})", version.label()))
}

/// Run a NAMED subset of the fixed case set against both versions, with a
/// per-call credential env var (parallel-test isolation).
async fn run_subset(cases: &[&str], ledger_env: &str) -> RegressionReport {
    let server = spawn_server().await;
    let all = fixed_case_set();
    let selected: Vec<bpm_engine_ai_reference::eval::RegressionCase> = cases
        .iter()
        .map(|name| {
            all.iter()
                .find(|c| c.name == *name)
                .unwrap_or_else(|| panic!("unknown case '{name}'"))
                .clone()
        })
        .collect();
    let provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::new());
    let selector = fixture_version_selector();
    std::env::set_var(ledger_env, "sk-eval-not-a-real-secret");
    let report = run_regression_with(
        &server.base_url,
        server.state.storage.deployer.as_ref(),
        provider,
        &selected,
        &selector,
        eval_options(ledger_env),
    )
    .await
    .expect("regression eval run");
    std::env::remove_var(ledger_env);
    server.stop().await;
    report
}

// ---------------------------------------------------------------------------
// The deterministic subset: three cases × both versions
// ---------------------------------------------------------------------------

/// Five representative cases × v1 and v2: the model-routed inference
/// (exact cost per the mock price table, BOTH gateway branches — the
/// low-risk verdict pins the other half of the behavior-preserving v2
/// refactor), the approval intervention counting, the transient-retry
/// recovery case whose v2 fixture (retries 3 → 1) is the
/// behavior-changing regression the comparison must surface, and the
/// permanent-failure no-retry-storm case (blocked + DLQ on the FIRST
/// attempt, both versions).
#[tokio::test]
async fn deterministic_subset_invariants_hold_for_both_versions() {
    let report = run_subset(
        &[
            "inference-high-risk",
            "inference-low-risk",
            "approval-approved",
            "tool-transient-failure",
            "retrieval-permanent-failure",
        ],
        &format!("{EVAL_LEDGER_ENV_BASE}_SUBSET"),
    )
    .await;

    // ---- structural validity: both versions present, ten runs ----------
    assert_eq!(report.versions, ["v1", "v2"]);
    assert_eq!(report.per_case.len(), 10, "5 cases × 2 versions");
    assert_eq!(report.summary.len(), 2);
    assert_eq!(report.comparison.baseline, "v1");
    assert_eq!(report.comparison.candidate, "v2");
    assert_eq!(report.comparison.per_case.len(), 5);
    for run in &report.per_case {
        assert!(run.steps > 0, "every run has steps");
        assert!(run.attempts > 0, "every run has at least one attempt");
        assert!(run.latency_ms < 5_000, "sanity only, not a wall-clock gate");
    }

    // ---- 推理 inference-high-risk: the model verdict routes the gateway,
    //       identically under the v2 gateway refactor ----------------------
    for version in Version::all() {
        let run = run_of(&report, "inference-high-risk", version);
        assert!(
            run.completed,
            "{}: state must be COMPLETED",
            version.label()
        );
        // The mock classifier's high-risk verdict is "rejected": the
        // rejected branch and end. v2 flips the gateway's default but
        // routes the same verdicts to the same ends (behavior-preserving
        // refactor by fixture design).
        assert_eq!(run.terminal_nodes, ["end-rejected"], "{}", version.label());
        assert_eq!(run.steps, 5, "{}", version.label());
        // ONE model attempt, one confirmation task.
        assert_eq!(run.attempts, 2, "{}", version.label());
        assert_eq!(run.failed_attempts, 0);
        // EXACT cost per the mock price table: classify = (24 * 250 + 2 *
        // 500) / 1000 = 7 micro-USD, 24 in / 2 out tokens.
        assert_eq!(run.cost_micros, 7, "{}", version.label());
        assert_eq!(run.tokens_in, 24);
        assert_eq!(run.tokens_out, 2);
        assert_eq!(run.intervention_points, 0, "no human touchpoints");
        assert!(!run.dlq_entry, "no dead letter");
        assert!(run.recovery.is_none(), "no injection");
        assert!(run.expectation_met, "{}", version.label());
    }

    // ---- 推理 inference-low-risk: the OTHER gateway branch — pins the
    //       second half of the behavior-preserving v2 refactor ------------
    for version in Version::all() {
        let run = run_of(&report, "inference-low-risk", version);
        assert!(run.completed, "{}", version.label());
        // The mock classifier's low-risk verdict is "approved" — the
        // approved branch. v2 flips the gateway default but routes the
        // same verdict to the same end (both halves of the refactor now
        // pinned deterministically).
        assert_eq!(run.terminal_nodes, ["end-approved"], "{}", version.label());
        assert_eq!(run.steps, 5, "{}", version.label());
        assert_eq!(run.attempts, 2, "{}", version.label());
        assert_eq!(run.failed_attempts, 0);
        assert_eq!(run.cost_micros, 7, "{}", version.label());
        assert_eq!(run.tokens_in, 24);
        assert_eq!(run.tokens_out, 2);
        assert_eq!(run.intervention_points, 0);
        assert!(!run.dlq_entry);
        assert!(run.recovery.is_none());
        assert!(run.expectation_met, "{}", version.label());
    }

    // ---- 审批 approval-approved: the intervention counting ----------------
    for version in Version::all() {
        let run = run_of(&report, "approval-approved", version);
        assert!(run.completed, "{}", version.label());
        assert_eq!(run.terminal_nodes, ["end"], "{}", version.label());
        // The user-task decision plus the `__approval` side-effect input
        // over the user task plus the act gate.
        assert_eq!(run.intervention_points, 2, "{}", version.label());
        assert_eq!(run.decision_points, 2, "{}", version.label());
        // propose (model) + act (tool) attempts; v2 adds the validate step:
        // propose + validate + act. The tool envelope carries no cost, so
        // the cost is the propose model call: (16 * 250 + 8 * 500) / 1000
        // = 8 micro-USD, 16/8 tokens.
        let expected_attempts = if version == Version::V1 { 2 } else { 3 };
        assert_eq!(run.attempts, expected_attempts, "{}", version.label());
        assert_eq!(run.cost_micros, 8, "{}", version.label());
        assert_eq!(run.tokens_in, 16, "{}", version.label());
        assert_eq!(run.tokens_out, 8, "{}", version.label());
        assert!(run.expectation_met, "{}", version.label());
    }
    // The v2 approval definition adds the validation step: one more step,
    // one more attempt, NO extra model cost.
    let v1 = run_of(&report, "approval-approved", Version::V1);
    let v2 = run_of(&report, "approval-approved", Version::V2);
    assert_eq!(v1.steps + 1, v2.steps, "the v2 validation step");
    assert_eq!(v1.attempts + 1, v2.attempts);
    assert_eq!(v1.cost_micros, v2.cost_micros, "no extra model call");
    assert_eq!(v1.process_def_id, "ai-reference-approval");
    assert_eq!(v2.process_def_id, "ai-reference-approval:2");

    // ---- 工具 tool-transient-failure: THE behavior-changing fixture delta --
    // v1 (retries 3): the injected transient failure consumes one retry;
    // the redelivered attempt completes the flow — recovery correct.
    let v1 = run_of(&report, "tool-transient-failure", Version::V1);
    assert!(v1.completed, "v1 must complete after the retry");
    assert_eq!(v1.terminal_nodes, ["end"]);
    assert_eq!(v1.steps, 3, "start, run-tool, end");
    assert_eq!(v1.attempts, 2, "exactly one failed + one completed attempt");
    assert_eq!(v1.failed_attempts, 1);
    assert!(v1.recovery.as_ref().expect("injected").correct);
    let recovery = v1.recovery.as_ref().unwrap();
    assert_eq!(recovery.node_attempts, 2);
    assert_eq!(recovery.node_failed_attempts, 1);
    assert!(!recovery.dlq_entry);
    assert!(!v1.dlq_entry);
    assert!(v1.expectation_met);
    // The tool envelope records no cost — the total is exactly zero.
    assert_eq!(v1.cost_micros, 0);

    // v2 (retries 1): the SAME single transient failure now exhausts the
    // budget — Failed + dead letter, the instance stays blocked at
    // run-tool. The regression the comparison must surface.
    let v2 = run_of(&report, "tool-transient-failure", Version::V2);
    assert!(!v2.completed, "v2 must be blocked by the exhausted retry");
    assert!(v2.terminal_nodes.is_empty());
    assert_eq!(v2.steps, 2, "start, run-tool (no end reached)");
    assert_eq!(v2.attempts, 1, "no retry storm: exactly one attempt");
    assert_eq!(v2.failed_attempts, 1);
    assert!(v2.dlq_entry, "the exhausted task dead-letters");
    assert!(!v2.recovery.as_ref().expect("injected").correct);
    assert!(!v2.expectation_met, "v1 expectations are unmet under v2");
    assert_eq!(v2.process_def_id, "ai-reference-tool:2");

    // ---- 恢复 retrieval-permanent-failure: the no-retry-storm case ---------
    // A permanent failure must go straight to Failed + DLQ on the FIRST
    // attempt with no retry storm, in BOTH versions.
    for version in Version::all() {
        let run = run_of(&report, "retrieval-permanent-failure", version);
        assert!(!run.completed, "{}: permanently blocked", version.label());
        assert!(run.terminal_nodes.is_empty());
        assert_eq!(
            run.steps,
            2,
            "{}: start + ref-retrieve, no end",
            version.label()
        );
        assert_eq!(run.attempts, 1, "{}: exactly one attempt", version.label());
        assert_eq!(run.failed_attempts, 1);
        assert!(
            run.dlq_entry,
            "{}: the permanent task dead-letters",
            version.label()
        );
        let recovery = run.recovery.as_ref().expect("injected");
        assert!(
            recovery.correct,
            "{}: permanent → no retry storm is CORRECT",
            version.label()
        );
        assert_eq!(recovery.node_attempts, 1);
        assert_eq!(recovery.node_failed_attempts, 1);
        assert!(recovery.dlq_entry);
        assert_eq!(run.cost_micros, 0, "no model cost on a failed retrieval");
        assert!(run.expectation_met, "{}", version.label());
    }

    // ---- the comparison delta fields EXIST and are deterministic --------
    // v1: 4/5 completed; v2: 3/5 (the tool case regressed; the permanent
    // case is blocked in both by design). The exact delta is a
    // deterministic function of the cases run.
    let s1 = &report.summary[0];
    let s2 = &report.summary[1];
    assert_eq!(s1.completed, 4);
    assert_eq!(s2.completed, 3);
    assert_eq!(s1.success_rate, 0.8);
    assert!((s2.success_rate - 0.6).abs() < 1e-9);
    assert!((report.comparison.success_rate_delta + 0.2).abs() < 1e-9);
    // Two injection cases: v1 both correct (transient retried + permanent
    // no-storm); v2 only the permanent one (the tightened retry budget
    // regressed the transient case).
    assert_eq!(s1.recovery_rate, Some(1.0));
    assert_eq!(s2.recovery_rate, Some(0.5));
    assert!((report.comparison.recovery_rate_delta.unwrap() + 0.5).abs() < 1e-9);
    // Intervention is unchanged by these definition edits.
    assert_eq!(
        report.comparison.intervention_rate_delta, 0.0,
        "no human-touchpoint change in these fixtures"
    );
    // Costs: 7 + 8 + 0 per version — identical totals.
    assert_eq!(report.comparison.cost_delta_micros, 0);

    // ---- the report JSON round-trips ---------------------------------------
    let json = serde_json::to_value(&report).expect("serialize report");
    let back: RegressionReport = serde_json::from_value(json).expect("deserialize report");
    assert_eq!(back, report, "the report round-trips losslessly");

    // ---- the markdown summary names both versions and the split ----------
    let markdown = report.markdown_summary();
    assert!(markdown.contains("## Per-version metrics"));
    assert!(markdown.contains("| v1 |"));
    assert!(markdown.contains("| v2 |"));
    assert!(markdown.contains("## v1 → v2 deltas"));
    assert!(markdown.contains("reported only"), "the split is stated");
}

// ---------------------------------------------------------------------------
// The crash-redelivery case (its own test: the crash window takes seconds)
// ---------------------------------------------------------------------------

/// A worker that aborts mid-handle (lease expiry, no fail submitted) must
/// be redelivered and complete — in BOTH versions (the v2 retrieval
/// fixture's added validation step does not disturb the recovery path).
#[tokio::test]
async fn worker_crash_redelivers_and_completes_both_versions() {
    let report = run_subset(
        &["retrieval-worker-crash"],
        &format!("{EVAL_LEDGER_ENV_BASE}_CRASH"),
    )
    .await;
    for version in Version::all() {
        let run = run_of(&report, "retrieval-worker-crash", version);
        // Completed through the redelivered attempt.
        assert!(run.completed, "{}", version.label());
        assert_eq!(run.terminal_nodes, ["end"], "{}", version.label());
        // Exactly two lease attempts on the injected retrieve task: the
        // crashed one (expired — NO fail event, no retry consumed) and the
        // redelivered one that completed; plus the summarize task's own
        // attempt (and v2's validation step) — three in total for v1, four
        // for v2.
        let expected_attempts = if version == Version::V1 { 3 } else { 4 };
        assert_eq!(run.attempts, expected_attempts, "{}", version.label());
        assert_eq!(run.failed_attempts, 0, "expiry is not a failure");
        assert!(!run.dlq_entry, "{}", version.label());
        let recovery = run.recovery.as_ref().expect("injected case");
        assert!(recovery.correct, "{}: {recovery:?}", version.label());
        assert_eq!(recovery.node_attempts, 2);
        assert_eq!(recovery.node_failed_attempts, 0);
        assert!(recovery.node_completed);
        // The summarize model call still cost exactly 18 micro-USD (48/12
        // tokens per the mock price table) — the crash did not double it.
        assert_eq!(run.cost_micros, 18, "{}", version.label());
        assert_eq!(run.tokens_in, 48);
        assert_eq!(run.tokens_out, 12);
        assert!(run.expectation_met, "{}", version.label());
    }
    // v2 adds the validation step after the summary.
    let v1 = run_of(&report, "retrieval-worker-crash", Version::V1);
    let v2 = run_of(&report, "retrieval-worker-crash", Version::V2);
    assert_eq!(v1.steps + 1, v2.steps);
    // The injection error never recorded a fail: the error channel stays
    // clean (asserted through failed_attempts == 0 above).
    assert_eq!(v1.failed_attempts, 0);
    assert_eq!(v2.failed_attempts, 0);
}
