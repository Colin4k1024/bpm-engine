//! Step-trace REST integration tests (E6-1, issue #26), memory mode.
//!
//! Reuses the E1-1 harness pattern (`build_app` + ephemeral port + real
//! HTTP, like `ai_task_envelope.rs` / `scheduler_workers.rs`). Covers the
//! `GET /api/v1/process-instances/:id/steps` endpoint:
//!
//! - happy path: two worker attempts on one external-task step (worker A
//!   fails with an envelope incl. model/tool/cost, worker B completes with
//!   cost + artifacts), steps in causal order, `trace_id == instance_id`,
//!   and NO variable values anywhere in the response body — including the
//!   envelope's input/output SCHEMA fragments (keys and content);
//! - expired-lease redelivery (E1-3): worker A's 1s lease elapses with no
//!   outcome, worker B re-locks and completes — attempts[0].outcome is
//!   `expired` (null outcome timestamp), attempts[1].outcome `completed`;
//! - redaction: an oversized envelope degrades to `{"opaque": true}` in
//!   the attempt and node summaries; a sensitive (encrypted) variable
//!   value never appears;
//! - `not_found` for an unknown instance id;
//! - a timer flow shows the timer node step;
//! - the live response satisfies every `required` field of the
//!   `StepsResponse` schema family in `docs/openapi.yaml`.
//!
//! Memory driver — runs in CI by default (no Docker).

use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, EngineConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// One external task (type "ai-work"): retries once (worker A fails,
/// worker B completes).
const AI_WORK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="ai-work-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="ai-work" name="AI Work">
      <extensionElements>
        <taskDefinition type="ai-work"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="ai-work"/>
    <sequenceFlow id="f2" sourceRef="ai-work" targetRef="end"/>
  </process>
</definitions>"#;

/// Process with a PT1S timer intermediate catch event (fixture pattern
/// from `scheduler_workers.rs` / tests/fixtures/timer_events.bpmn).
const TIMER_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="timer-catch-flow" isExecutable="true">
    <startEvent id="start"/>
    <intermediateCatchEvent id="wait-1s" name="Wait 1 Second">
      <timerEventDefinition>
        <timeDuration>PT1S</timeDuration>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="wait-1s"/>
    <sequenceFlow id="f2" sourceRef="wait-1s" targetRef="end"/>
  </process>
</definitions>"#;

/// Distinctive plaintext values the /steps response must NEVER contain.
const SECRET_START_VALUE: &str = "step-trace-secret-start-42";
const SECRET_OUTPUT_VALUE: &str = "Q3 revenue grew 12%.";
const ENVELOPE_IDEMPOTENCY_KEY: &str = "sha256:step-trace|gpt-4o|web_search";
/// Distinctive markers INSIDE the envelope's input/output schema
/// fragments — the schema keys and any of their content must never
/// surface in /steps (the summary is metadata only).
const INPUT_SCHEMA_MARKER: &str = "input-schema-fragment-step-trace-42";
const OUTPUT_SCHEMA_MARKER: &str = "output-schema-fragment-step-trace-42";

/// The only test in this binary that manipulates the process-global
/// BPM_VARIABLE_ENCRYPTION_KEY (pattern: variable_contract.rs).
const TEST_KEY_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The OpenAPI contract, embedded from the repo like the consistency
/// suite does (no runtime file dependency).
const OPENAPI_YAML: &str = include_str!("../../../../docs/openapi.yaml");

struct TestServer {
    base_url: String,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

/// Build the app and serve it on an ephemeral port (fast timer poll so
/// the timer test is quick but still exercises the real scheduler).
async fn spawn_server(config: &BpmConfig) -> TestServer {
    let app = build_app(config).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app.router).await;
    });
    TestServer {
        base_url: format!("http://{}", addr),
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

fn memory_config() -> BpmConfig {
    BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        engine: EngineConfig {
            timer_poll_interval_ms: 200,
            timer_batch_size: 100,
            ..Default::default()
        },
        // Poll loops and multi-request flows stay clear of the limiter.
        rate_limit: RateLimitConfig {
            requests_per_minute: 10_000,
        },
        ..Default::default()
    }
}

async fn deploy(client: &reqwest::Client, server: &TestServer, bpmn: &str) {
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(bpmn.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "deploy should return 201");
}

async fn start_instance(
    client: &reqwest::Client,
    server: &TestServer,
    process_def_id: &str,
    variables: serde_json::Value,
) -> String {
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": process_def_id,
            "variables": variables,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "start should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["instance_id"].as_str().unwrap().to_string()
}

async fn get_instance(
    client: &reqwest::Client,
    server: &TestServer,
    instance_id: &str,
) -> serde_json::Value {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

/// GET /steps and the raw response body string (for value-leak checks).
async fn get_steps(
    client: &reqwest::Client,
    server: &TestServer,
    instance_id: &str,
) -> (reqwest::StatusCode, serde_json::Value, String) {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/steps",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let raw = resp.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
    (status, body, raw)
}

/// Fetch and lock the one external task of `task_type` via raw HTTP.
async fn fetch_and_lock(
    client: &reqwest::Client,
    server: &TestServer,
    worker_id: &str,
    task_type: &str,
    lock_duration_ms: u64,
) -> serde_json::Value {
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .json(&serde_json::json!({
            "worker_id": worker_id,
            "task_types": [task_type],
            "max_tasks": 1,
            "lock_duration_ms": lock_duration_ms,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tasks: serde_json::Value = resp.json().await.unwrap();
    let tasks = tasks.as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1, "worker should lock the {task_type} task");
    tasks[0].clone()
}

/// A full envelope body with model, tools and cost (E6-1).
fn envelope_json(execution_id: &str, with_cost: bool) -> serde_json::Value {
    let mut envelope = serde_json::json!({
        "schema_version": 1,
        "execution_id": execution_id,
        "idempotency_key": ENVELOPE_IDEMPOTENCY_KEY,
        "timeout_secs": 120,
        "model": {
            "provider": "openai",
            "name": "gpt-4o",
            "version": "2024-08-06"
        },
        "tools": [ { "name": "web_search", "version": "2.1" } ],
        "artifacts": [
            {
                "uri": "https://storage.example.com/out/report.md",
                "sha256": "b".repeat(64),
                "size": 4096,
                "media_type": "text/markdown"
            }
        ],
        "input_schema": {
            "type": "object",
            "properties": { "topic": { "type": "string" } },
            "required": ["topic"],
            "x-marker": INPUT_SCHEMA_MARKER
        },
        "output_schema": {
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "x-marker": OUTPUT_SCHEMA_MARKER
        }
    });
    if with_cost {
        envelope["cost"] = serde_json::json!({
            "currency": "USD",
            "amount_micros": 1_500_000,
            "tokens_in": 1_204,
            "tokens_out": 37,
            "duration_ms": 980
        });
    }
    envelope
}

async fn wait_for_state(
    client: &reqwest::Client,
    server: &TestServer,
    instance_id: &str,
    expected: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let body = get_instance(client, server, instance_id).await;
        if body["status"] == expected {
            return body;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "instance {} did not reach {} within {:?}; last state: {}",
                instance_id, expected, timeout, body["status"]
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The external-task step of a /steps response (exactly one).
fn task_step(body: &serde_json::Value) -> &serde_json::Value {
    let steps = body["steps"].as_array().expect("steps array");
    steps
        .iter()
        .find(|s| s["kind"] == "external_task")
        .unwrap_or_else(|| panic!("no external_task step in: {body}"))
}

// ---------------------------------------------------------------------------
// Happy path: two attempts, envelopes joined, no variable values
// ---------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_two_attempts_join_envelopes_and_hide_variables() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(
        &client,
        &server,
        "ai-work-flow",
        serde_json::json!({ "topic": SECRET_START_VALUE }),
    )
    .await;

    // Worker A: 1s lease, fails once (transient) WITH an envelope.
    let task_a = fetch_and_lock(&client, &server, "worker-a", "ai-work", 1_000).await;
    let task_id = task_a["task_id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/fail",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "worker-a",
            "error": "model over quota",
            "retry_after_ms": 100,
            "category": "transient",
            "envelope": envelope_json(&format!("{}:0", task_id), true),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "worker-a fail: {resp:?}");

    // Worker B: fetches the retried task, completes WITH an envelope
    // (cost + artifacts) and output variables.
    let task_b = fetch_and_lock(&client, &server, "worker-b", "ai-work", 60_000).await;
    assert_eq!(task_b["task_id"].as_str().unwrap(), task_id);
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "worker-b",
            "variables": { "summary": SECRET_OUTPUT_VALUE },
            "envelope": envelope_json(&format!("{}:1", task_id), true),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "worker-b complete: {resp:?}");
    wait_for_state(
        &client,
        &server,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(10),
    )
    .await;

    let (status, body, raw) = get_steps(&client, &server, &instance_id).await;
    assert_eq!(status, 200, "steps: {raw}");

    // Trace id IS the instance id (v1 decision).
    assert_eq!(body["trace_id"], serde_json::json!(instance_id));
    assert_eq!(body["instance_id"], serde_json::json!(instance_id));
    assert_eq!(body["process_def_id"], "ai-work-flow");
    assert_eq!(body["state"], "COMPLETED");

    // Steps in causal order: start → external task → end.
    let steps = body["steps"].as_array().expect("steps array");
    let node_ids: Vec<&str> = steps
        .iter()
        .map(|s| s["node_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        node_ids,
        vec!["start", "ai-work", "end"],
        "causal order: {body}"
    );
    assert_eq!(
        steps
            .iter()
            .map(|s| s["step_no"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "step_no is 1-based causal position"
    );
    assert_eq!(steps[1]["kind"], "external_task");
    assert_eq!(steps[0]["kind"], "node");
    assert_eq!(steps[2]["kind"], "node");
    // Every step carries its events (types + timestamps only).
    for step in steps {
        assert!(
            step["events"].as_array().is_some_and(|e| !e.is_empty()),
            "each step has events: {step}"
        );
    }
    // The task step's variables_touched names the completed output
    // variable — the NAME only, never the value; the engine-owned
    // `__task_envelope` key is excluded (it is reported through the
    // envelope summaries). The start step touched no step-scoped
    // variables (initial variables ride the instance-level
    // ProcessStarted event, which is not a step).
    assert_eq!(
        steps[1]["variables_touched"],
        serde_json::json!(["summary"])
    );
    assert!(
        steps[0].get("variables_touched").is_none(),
        "empty lists are omitted: {steps:?}"
    );

    // Two attempts on the task step: A failed (error + envelope incl.
    // model/cost), B completed.
    let step = task_step(&body);
    let attempts = step["attempts"].as_array().expect("attempts array");
    assert_eq!(attempts.len(), 2, "two lease attempts: {step}");

    let a = &attempts[0];
    assert_eq!(a["worker_id"], "worker-a");
    assert_eq!(a["outcome"], "failed");
    assert!(a["locked_at"].is_string(), "locked_at timestamp");
    assert!(
        a["completed_or_failed"].is_string(),
        "the failed attempt carries the fail timestamp"
    );
    assert!(
        a["error"].as_str().unwrap().contains("model over quota"),
        "the failed attempt carries the error: {a}"
    );
    assert!(a["retries_remaining"].is_u64(), "retries recorded: {a}");
    let a_envelope = &a["envelope"];
    assert_eq!(a_envelope["model"]["name"], "gpt-4o");
    assert_eq!(a_envelope["model"]["provider"], "openai");
    assert_eq!(a_envelope["tools"][0]["name"], "web_search");
    assert_eq!(a_envelope["cost"]["currency"], "USD");
    assert_eq!(a_envelope["cost"]["amount_micros"], 1_500_000);
    assert_eq!(a_envelope["cost"]["tokens_in"], 1_204);
    assert_eq!(a_envelope["retry_category"], "transient");
    assert_eq!(a_envelope["execution_id"], format!("{}:0", task_id));

    let b = &attempts[1];
    assert_eq!(b["worker_id"], "worker-b");
    assert_eq!(b["outcome"], "completed");
    assert!(
        b["completed_or_failed"].is_string(),
        "the completed attempt carries the complete timestamp"
    );
    assert!(b["error"].is_null(), "no error on the completed attempt");
    let b_envelope = &b["envelope"];
    assert_eq!(b_envelope["cost"]["tokens_out"], 37);
    assert_eq!(
        b_envelope["artifacts"][0]["uri"],
        "https://storage.example.com/out/report.md"
    );
    assert_eq!(b_envelope["execution_id"], format!("{}:1", task_id));

    // Node-level envelope summary: the LAST parsable envelope (worker B's).
    let summary = &step["envelope_summary"];
    assert_eq!(summary["execution_id"], format!("{}:1", task_id));
    assert_eq!(summary["cost"]["amount_micros"], 1_500_000);

    // Sensitive-by-default: no variable values anywhere in the response.
    assert!(
        !raw.contains(SECRET_START_VALUE),
        "the start variable value must not appear in /steps"
    );
    assert!(
        !raw.contains(SECRET_OUTPUT_VALUE),
        "the output variable value must not appear in /steps"
    );
    assert!(
        !raw.contains(ENVELOPE_IDEMPOTENCY_KEY),
        "the envelope summary carries no idempotency key"
    );
    assert!(
        !raw.contains("__task_envelope"),
        "the reserved envelope key/value itself never surfaces"
    );
    // The sent envelopes CARRIED input/output schema fragments with
    // recognizable markers — the schemas are excluded from the summary,
    // so neither their keys nor any of their content may appear.
    assert!(
        !raw.contains("input_schema"),
        "the input_schema key never surfaces: {raw}"
    );
    assert!(
        !raw.contains("output_schema"),
        "the output_schema key never surfaces: {raw}"
    );
    assert!(
        !raw.contains(INPUT_SCHEMA_MARKER),
        "input schema content never surfaces: {raw}"
    );
    assert!(
        !raw.contains(OUTPUT_SCHEMA_MARKER),
        "output schema content never surfaces: {raw}"
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Expired-lease redelivery (E1-3): the superseded attempt is `expired`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_lease_redelivery_marks_first_attempt_expired() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({})).await;

    // Worker A takes a 1s lease and never resolves it (crash / hang).
    let task_a = fetch_and_lock(&client, &server, "worker-a", "ai-work", 1_000).await;
    let task_id = task_a["task_id"].as_str().unwrap().to_string();

    // The lease elapses; worker B re-locks the SAME task. No
    // ExternalTaskFailed/Completed event ever exists for worker A — the
    // history shows two locks with no outcome between them.
    tokio::time::sleep(Duration::from_millis(1_400)).await;
    let task_b = fetch_and_lock(&client, &server, "worker-b", "ai-work", 60_000).await;
    assert_eq!(
        task_b["task_id"].as_str().unwrap(),
        task_id,
        "the expired lease is reclaimed and re-locked"
    );

    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "worker-b",
            "variables": { "summary": "done after redelivery" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "worker-b complete: {resp:?}");
    wait_for_state(
        &client,
        &server,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(10),
    )
    .await;

    let (status, body, raw) = get_steps(&client, &server, &instance_id).await;
    assert_eq!(status, 200, "steps: {raw}");
    let step = task_step(&body);
    let attempts = step["attempts"].as_array().expect("attempts array");
    assert_eq!(attempts.len(), 2, "one attempt per lock: {step}");

    // Worker A's attempt is closed as EXPIRED — not mislabeled as in
    // flight — and keeps the null outcome timestamp (no terminating
    // event exists for it).
    assert_eq!(attempts[0]["worker_id"], "worker-a");
    assert_eq!(
        attempts[0]["outcome"], "expired",
        "the superseded lease is expired: {attempts:?}"
    );
    assert!(
        attempts[0]["completed_or_failed"].is_null(),
        "expired has no terminating event: {attempts:?}"
    );
    // Worker B's attempt completed normally.
    assert_eq!(attempts[1]["worker_id"], "worker-b");
    assert_eq!(attempts[1]["outcome"], "completed");
    assert!(attempts[1]["completed_or_failed"].is_string());

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Redaction: opaque envelope summary, encrypted value never surfaces
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oversized_envelope_is_opaque_and_sensitive_value_never_surfaces() {
    // The ONLY test in this binary that manipulates the process-global
    // BPM_VARIABLE_ENCRYPTION_KEY (pattern: variable_contract.rs).
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    std::env::set_var("BPM_VARIABLE_ENCRYPTION_KEY", TEST_KEY_HEX);

    // Start with a sensitive (⇒ encrypted) variable: the /steps endpoint
    // must not contain the plaintext even though GET /process-instances
    // would decrypt it.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "ai-work-flow",
            "variables": { "secret": "swordfish-trace", "topic": "public-topic" },
            "encrypted": ["secret"],
            "sensitive": ["secret"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "sensitive+encrypted must be accepted");
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    let task = fetch_and_lock(&client, &server, "worker-redact", "ai-work", 60_000).await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // An envelope whose serialized form is ~5 KB: passes the boundary cap
    // but exceeds the history redaction cap, so the recorded history value
    // is a __truncated: marker the assembler must treat as opaque.
    let mut envelope = envelope_json(&format!("{}:0", task_id), true);
    envelope["output_schema"]["properties"]["filler"] = serde_json::json!("x".repeat(5000));
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "worker-redact",
            "variables": { "summary": SECRET_OUTPUT_VALUE },
            "envelope": envelope,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "oversized-history envelope: {resp:?}");

    let (status, steps_body, raw) = get_steps(&client, &server, &instance_id).await;
    assert_eq!(status, 200);

    // The attempt and node summaries degrade to the opaque marker.
    let step = task_step(&steps_body);
    let attempt = &step["attempts"][0];
    assert_eq!(
        attempt["envelope"],
        serde_json::json!({ "opaque": true }),
        "oversized (redacted) envelope ⇒ opaque summary: {attempt}"
    );
    assert_eq!(
        step["envelope_summary"],
        serde_json::json!({ "opaque": true }),
        "node-level summary is opaque too: {step}"
    );

    // The encrypted variable's plaintext never appears (history keeps
    // ciphertext; /steps reads no values at all).
    assert!(!raw.contains("swordfish-trace"));
    assert!(!raw.contains(SECRET_OUTPUT_VALUE));
    assert!(!raw.contains("__truncated"), "markers never leak: {raw}");

    std::env::remove_var("BPM_VARIABLE_ENCRYPTION_KEY");
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Error path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_instance_answers_not_found() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    let (status, body, _) =
        get_steps(&client, &server, "00000000-0000-4000-8000-000000000000").await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "not_found");
    assert!(body["error"].as_str().unwrap().contains("process instance"));

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Timer flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn timer_flow_shows_the_timer_step() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, TIMER_BPMN).await;
    let instance_id =
        start_instance(&client, &server, "timer-catch-flow", serde_json::json!({})).await;

    // The PT1S timer fires through the real scheduler (200 ms poll).
    wait_for_state(
        &client,
        &server,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(10),
    )
    .await;

    let (status, body, _raw) = get_steps(&client, &server, &instance_id).await;
    assert_eq!(status, 200);
    assert_eq!(body["trace_id"], serde_json::json!(instance_id));

    // The timer node appears as a `timer` step. Honest v1: the engine
    // records the timer token's arrival (TimerArrived event) but does not
    // emit `TimerScheduled`, and `TimerFired` is written with an EMPTY
    // instance id by the engine's history writer (existing behavior, out
    // of E6-1 scope) — so the step shows the token waiting at the timer
    // node, and the flow continuing past it.
    let steps = body["steps"].as_array().expect("steps array");
    let timer_step = steps
        .iter()
        .find(|s| s["node_id"] == "wait-1s")
        .unwrap_or_else(|| panic!("no wait-1s step in: {body}"));
    assert_eq!(timer_step["kind"], "timer");
    let event_types: Vec<&str> = timer_step["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert!(
        event_types.contains(&"TokenArrived"),
        "timer step carries the token arrival: {timer_step}"
    );
    // Causal order: start → timer → end.
    let node_ids: Vec<&str> = steps
        .iter()
        .map(|s| s["node_id"].as_str().unwrap())
        .collect();
    assert_eq!(node_ids, vec!["start", "wait-1s", "end"]);
    // The step identity is (node, token): the timer token differs from
    // the start token.
    assert_ne!(steps[0]["token_id"], steps[1]["token_id"]);

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Spec sample-response validation (required fields of the openapi.yaml
// StepsResponse schema family; the full subset validator lives in the
// openapi_contract suite)
// ---------------------------------------------------------------------------

/// Recursively assert that every `required` field named by `schema` (and
/// by the `$ref` targets it reaches through `properties`/`items`) exists
/// in `value`. Covers the whitelisted subset the spec uses for these
/// schemas (`$ref`/`type`/`required`/`properties`/`items`).
fn assert_required_fields(
    spec: &serde_json::Value,
    schema: &serde_json::Value,
    value: &serde_json::Value,
    path: &str,
) {
    let schema = if let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
        let pointer = reference.trim_start_matches('#');
        spec.pointer(pointer)
            .unwrap_or_else(|| panic!("schema not found at {reference}"))
    } else {
        schema
    };
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("{path}: expected an object, got {value}"));
        for field in required {
            let field = field.as_str().unwrap();
            assert!(
                obj.contains_key(field),
                "{path}: required field {field:?} missing in {value}"
            );
        }
    }
    if let (Some(props), Some(obj)) = (
        schema.get("properties").and_then(|p| p.as_object()),
        value.as_object(),
    ) {
        for (name, prop_schema) in props {
            if let Some(actual) = obj.get(name) {
                assert_required_fields(spec, prop_schema, actual, &format!("{path}.{name}"));
            }
        }
    }
    if let Some(items) = schema.get("items") {
        if let Some(arr) = value.as_array() {
            for (idx, item) in arr.iter().enumerate() {
                assert_required_fields(spec, items, item, &format!("{path}[{idx}]"));
            }
        }
    }
}

#[tokio::test]
async fn sample_steps_response_satisfies_the_spec_required_fields() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({})).await;

    let task = fetch_and_lock(&client, &server, "worker-spec", "ai-work", 60_000).await;
    let task_id = task["task_id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "worker-spec",
            "variables": { "summary": "done" },
            "envelope": envelope_json(&format!("{}:0", task_id), true),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    wait_for_state(
        &client,
        &server,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(10),
    )
    .await;

    let (status, body, raw) = get_steps(&client, &server, &instance_id).await;
    assert_eq!(status, 200, "{raw}");
    let yaml: serde_yaml::Value = serde_yaml::from_str(OPENAPI_YAML).unwrap();
    let spec = serde_json::to_value(yaml).unwrap();
    let steps_schema = spec
        .pointer("/components/schemas/StepsResponse")
        .expect("StepsResponse schema in the spec");
    assert_required_fields(&spec, steps_schema, &body, "steps-response");

    server.stop().await;
}
