//! REST-level integration tests for the AI task envelope contract (E3-1,
//! issue #15), memory storage mode.
//!
//! Reuses the E1-1 harness pattern (`build_app` + ephemeral port + real
//! HTTP, like `scheduler_workers.rs` / `variable_contract.rs`). Covers:
//!
//! - complete WITH a typed envelope: merged into the reserved
//!   `__task_envelope` variable (task row + instance + history), E2-3 caps
//!   and history redaction applying to the merged map,
//! - complete with an INVALID envelope (artifact URI) → 4xx
//!   `task_envelope_invalid` (sub-code in the message), task untouched,
//! - complete supplying `__task_envelope` directly → 4xx
//!   `reserved_variable_key` (external-task complete, user-task complete,
//!   start-instance),
//! - complete WITHOUT an envelope → unchanged legacy behavior,
//! - fail with `category` (+ envelope): category recorded as a structured
//!   error-message prefix, envelope recorded (redacted) in the
//!   `ExternalTaskFailed` history payload, retry behavior unchanged,
//! - worker-sdk typed round trip: `TaskOutput` → `complete_with_envelope`
//!   → server-side parse via core → `TaskInput` on the chained task.

use bpm_engine_core::{AiTaskEnvelope, ExternalTaskState};
use bpm_engine_server_rest::bpm_config::{BpmConfig, DatabaseConfig, ServerConfig};
use bpm_engine_server_rest::build_app;
use bpm_engine_server_rest::state::AppState;
use bpm_engine_worker_sdk::ai_task::{new_envelope, TaskInput, TaskOutput};
use bpm_engine_worker_sdk::EngineClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// One external task (type "ai-work"): completes after the worker does.
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

/// Two chained external tasks ("ai-a" then "ai-b"): completing ai-a WITH an
/// envelope delivers that envelope to ai-b's input variables (the instance
/// merge happens before the ai-b task snapshot is taken).
const AI_CHAIN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="ai-chain-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="ai-a" name="AI Step A">
      <extensionElements>
        <taskDefinition type="ai-a"/>
      </extensionElements>
    </serviceTask>
    <serviceTask id="ai-b" name="AI Step B">
      <extensionElements>
        <taskDefinition type="ai-b"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="ai-a"/>
    <sequenceFlow id="f2" sourceRef="ai-a" targetRef="ai-b"/>
    <sequenceFlow id="f3" sourceRef="ai-b" targetRef="end"/>
  </process>
</definitions>"#;

/// A user task: blocks so the user-task complete boundary is reachable.
const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="user-task-probe" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;

struct TestServer {
    base_url: String,
    state: Arc<AppState>,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

/// Build the app and serve it on an ephemeral port.
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
        state: app.state,
        serve_task,
        timer_cancel: app.timer_cancel,
    }
}

impl TestServer {
    /// Stop the server: cancel background tasks, abort the serve task.
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
        ..Default::default()
    }
}

async fn deploy(client: &reqwest::Client, server: &TestServer, bpmn: &str) -> String {
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
    let body: serde_json::Value = resp.json().await.unwrap();
    body["process_definition_id"]
        .as_str()
        .expect("process_definition_id")
        .to_string()
}

async fn start_instance(
    client: &reqwest::Client,
    server: &TestServer,
    process_def_id: &str,
    variables: serde_json::Value,
) -> Result<String, (reqwest::StatusCode, serde_json::Value)> {
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": process_def_id,
            "variables": variables,
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    if status.is_success() {
        Ok(body["instance_id"].as_str().unwrap().to_string())
    } else {
        Err((status, body))
    }
}

/// Fetch and lock the one external task of `task_type` via raw HTTP.
async fn fetch_and_lock(
    client: &reqwest::Client,
    server: &TestServer,
    worker_id: &str,
    task_type: &str,
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
            "lock_duration_ms": 60_000,
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

async fn get_history(
    client: &reqwest::Client,
    server: &TestServer,
    instance_id: &str,
) -> serde_json::Value {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

/// The payload of the first history event with the given type.
fn history_payload(history: &serde_json::Value, event_type: &str) -> serde_json::Value {
    history["events"]
        .as_array()
        .expect("events array")
        .iter()
        .find(|e| e["event_type"] == event_type)
        .map(|e| e["payload"].clone())
        .unwrap_or_else(|| panic!("no {event_type} event in history: {history}"))
}

/// A full, valid envelope body as the REST `envelope` field carries it.
fn envelope_json(execution_id: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "execution_id": execution_id,
        "idempotency_key": "sha256:input|model|tools",
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
            "required": ["topic"]
        },
        "output_schema": {
            "type": "object",
            "properties": { "summary": { "type": "string" } }
        }
    })
}

/// Parse the envelope the instance/history carries at `__task_envelope`.
fn parse_stored_envelope(value: &str) -> AiTaskEnvelope {
    AiTaskEnvelope::from_envelope_value(value)
        .unwrap_or_else(|e| panic!("stored envelope must parse: {e}"))
}

// ---------------------------------------------------------------------------
// Complete with envelope: merge, storage, history, redaction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_with_envelope_round_trip() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-ai", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // Complete WITH the typed envelope field; payload variables ride along.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-ai",
            "variables": { "summary": "Q3 revenue grew 12%." },
            "envelope": envelope_json(&format!("{}:0", task_id)),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "complete: {resp:?}");

    // The task row's variables carry the envelope as a __var:1: value.
    let stored_task = server
        .state
        .storage
        .external_task_store
        .get(&task_id)
        .await
        .unwrap()
        .expect("task row");
    let task_envelope = stored_task
        .variables
        .get("__task_envelope")
        .expect("task variables contain __task_envelope");
    assert!(task_envelope.starts_with("__var:1:"));
    let parsed = parse_stored_envelope(task_envelope);
    let expected: AiTaskEnvelope =
        serde_json::from_value(envelope_json(&format!("{}:0", task_id))).unwrap();
    assert_eq!(parsed, expected);
    // The payload variable merged into the task row too.
    assert_eq!(
        stored_task.variables.get("summary"),
        Some(&"Q3 revenue grew 12%.".to_string())
    );

    // The instance variables carry it (merged by ExternalTaskCompleted).
    let instance = get_instance(&client, &server, &instance_id).await;
    let instance_envelope = instance["variables"]["__task_envelope"]
        .as_str()
        .expect("instance variables contain __task_envelope");
    assert_eq!(parse_stored_envelope(instance_envelope), expected);

    // The ExternalTaskCompleted history payload carries it verbatim
    // (small envelope: below the 1024-byte history cap).
    let history = get_history(&client, &server, &instance_id).await;
    let payload = history_payload(&history, "ExternalTaskCompleted");
    let history_envelope = payload["variables"]["__task_envelope"]
        .as_str()
        .expect("history payload carries the envelope");
    assert_eq!(history_envelope, instance_envelope);

    server.stop().await;
}

#[tokio::test]
async fn oversized_envelope_history_redaction() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-big", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // An envelope whose serialized form is ~5 KB: passes the 65_536-byte
    // boundary cap but exceeds the 1024-byte history cap.
    let mut envelope = envelope_json(&format!("{}:0", task_id));
    envelope["output_schema"]["properties"]["filler"] = serde_json::json!("x".repeat(5000));
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-big",
            "variables": {},
            "envelope": envelope,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "oversized-history envelope: {resp:?}");

    // Instance variables keep the full value; event history redacts it.
    let instance = get_instance(&client, &server, &instance_id).await;
    let full = instance["variables"]["__task_envelope"]
        .as_str()
        .expect("instance keeps the full envelope");
    assert!(full.len() > 1024);
    let history = get_history(&client, &server, &instance_id).await;
    let payload = history_payload(&history, "ExternalTaskCompleted");
    let redacted = payload["variables"]["__task_envelope"]
        .as_str()
        .expect("history payload carries the (redacted) envelope");
    assert!(
        redacted.starts_with("__truncated:"),
        "oversized envelope must be redacted in history, got {redacted}"
    );
    // Marker carries the original length and a 12-hex digest.
    assert!(redacted.starts_with(&format!("__truncated:{}:", full.len())));

    server.stop().await;
}

#[tokio::test]
async fn complete_with_invalid_envelope_rejected_and_task_still_completable() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let _instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-bad", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // Invalid artifact URI inside the envelope → 4xx task_envelope_invalid
    // with the stable sub-code in the message; the store is untouched.
    let mut envelope = envelope_json(&format!("{}:0", task_id));
    envelope["artifacts"][0]["uri"] = serde_json::json!("not-a-relative-uri");
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-bad",
            "variables": { "summary": "done" },
            "envelope": envelope,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "task_envelope_invalid");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("ai_task_envelope_invalid_artifact_ref"),
        "message must carry the stable sub-code: {error}"
    );

    // The task was not mutated: still locked by the same owner and
    // completable without an envelope.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-bad",
            "variables": { "summary": "done" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "task still completable: {resp:?}");

    server.stop().await;
}

#[tokio::test]
async fn complete_supplying_reserved_key_directly_rejected() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let _instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-res", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // A non-envelope value at the reserved key is rejected: the key is
    // engine-owned. (The worker-sdk wire format — a SERIALIZED envelope
    // value — is accepted; see the collision and wire-format tests below.)
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-res",
            "variables": { "__task_envelope": "hand-rolled" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "reserved_variable_key");
    assert!(error["error"].as_str().unwrap().contains("__task_envelope"));

    server.stop().await;
}

#[tokio::test]
async fn complete_envelope_collision_between_channels_rejected() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let _instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-coll", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // Both channels at once is ambiguous: exactly one must be used.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-coll",
            "variables": {
                "__task_envelope": AiTaskEnvelope::to_envelope_value(&new_envelope(&task_id, 0))
            },
            "envelope": envelope_json(&format!("{}:0", task_id)),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "reserved_variable_key");
    assert!(error["error"].as_str().unwrap().contains("collides"));

    server.stop().await;
}

#[tokio::test]
async fn complete_with_serialized_envelope_variable_accepted() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-wire", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // The worker-sdk wire format, exercised over raw HTTP: the envelope
    // serialized into the variables map (no typed field) — byte-identical
    // to a worker that "completed exactly as today".
    let mut envelope = new_envelope(&task_id, 0);
    envelope.idempotency_key = Some("sha256:input".to_string());
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-wire",
            "variables": {
                "summary": "done",
                "__task_envelope": envelope.to_envelope_value(),
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "serialized envelope: {resp:?}");

    let instance = get_instance(&client, &server, &instance_id).await;
    let stored = instance["variables"]["__task_envelope"]
        .as_str()
        .expect("instance carries the envelope");
    assert_eq!(parse_stored_envelope(stored), envelope);

    server.stop().await;
}

#[tokio::test]
async fn complete_without_envelope_is_unchanged() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-plain", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // Legacy body: no envelope field at all.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-plain",
            "variables": { "result": "ok" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "legacy complete: {resp:?}");

    let instance = get_instance(&client, &server, &instance_id).await;
    assert!(
        instance["variables"].get("__task_envelope").is_none(),
        "no envelope key without the envelope field"
    );
    assert_eq!(instance["variables"]["result"], "ok");
    let history = get_history(&client, &server, &instance_id).await;
    let payload = history_payload(&history, "ExternalTaskCompleted");
    assert!(payload["variables"].get("__task_envelope").is_none());

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Reserved-key guard at the other boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn start_instance_with_reserved_key_rejected() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;

    let (status, error) = start_instance(
        &client,
        &server,
        "ai-work-flow",
        serde_json::json!({ "__task_envelope": "spoofed" }),
    )
    .await
    .expect_err("reserved key must be rejected");
    assert_eq!(status, 400);
    assert_eq!(error["code"], "reserved_variable_key");

    server.stop().await;
}

#[tokio::test]
async fn user_task_complete_with_reserved_key_rejected() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, USER_TASK_BPMN).await;
    let instance_id = start_instance(&client, &server, "user-task-probe", serde_json::json!({}))
        .await
        .expect("start should succeed");

    // task_id is "<instance_id>:<node_id>" for user tasks.
    let task_id = format!("{}:review", instance_id);
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "variables": { "__task_envelope": "spoofed" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "reserved_variable_key");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Fail path: category + envelope (v1: recorded, retry behavior unchanged)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fail_with_category_and_envelope_recorded() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-fail", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // Fail WITH category + envelope: accepted (200).
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/fail",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-fail",
            "error": "model over quota",
            "retry_after_ms": 5_000,
            "category": "permanent",
            "envelope": envelope_json(&format!("{}:0", task_id)),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "fail with envelope: {resp:?}");

    // The category is recorded as a structured prefix on the persisted
    // error message (task row).
    let stored_task = server
        .state
        .storage
        .external_task_store
        .get(&task_id)
        .await
        .unwrap()
        .expect("task row");
    let error_message = stored_task.error_message.as_deref().expect("error_message");
    assert!(
        error_message.starts_with(r#"{"retry_category":"permanent"}: model over quota"#),
        "structured category prefix missing: {error_message}"
    );
    // v1: retry behavior UNCHANGED — the task returns to Ready (retries
    // remain), "permanent" does not shortcut to the DLQ.
    assert_eq!(
        stored_task.state,
        ExternalTaskState::Ready,
        "permanent category must not change retry behavior in v1"
    );
    // The store's fail path has no variables channel: the task's input
    // variables are unchanged (no envelope merged there).
    assert!(!stored_task.variables.contains_key("__task_envelope"));

    // The envelope is recorded (redacted like any history variable) in the
    // ExternalTaskFailed history payload, with the category folded in.
    let history = get_history(&client, &server, &instance_id).await;
    let payload = history_payload(&history, "ExternalTaskFailed");
    assert_eq!(
        payload["error_message"].as_str().unwrap(),
        error_message,
        "history error_message carries the structured prefix"
    );
    let history_envelope = payload["variables"]["__task_envelope"]
        .as_str()
        .expect("fail history carries the envelope");
    let parsed = parse_stored_envelope(history_envelope);
    // The category given on fail is folded into the envelope.
    assert_eq!(
        parsed.retry_category,
        Some(bpm_engine_core::RetryCategory::Permanent)
    );
    // Everything else round-trips unchanged.
    let mut expected: AiTaskEnvelope =
        serde_json::from_value(envelope_json(&format!("{}:0", task_id))).unwrap();
    expected.retry_category = Some(bpm_engine_core::RetryCategory::Permanent);
    assert_eq!(parsed, expected);

    server.stop().await;
}

#[tokio::test]
async fn fail_with_invalid_category_or_envelope_rejected() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_WORK_BPMN).await;
    let _instance_id = start_instance(&client, &server, "ai-work-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let task = fetch_and_lock(&client, &server, "w-badcat", "ai-work").await;
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // Unknown category → 4xx invalid_retry_category.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/fail",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-badcat",
            "error": "boom",
            "category": "sometimes",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "invalid_retry_category");

    // Invalid envelope on fail → 4xx task_envelope_invalid (same rules as
    // complete), task untouched.
    let mut envelope = envelope_json(&format!("{}:0", task_id));
    envelope["schema_version"] = serde_json::json!(2);
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/fail",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-badcat",
            "error": "boom",
            "envelope": envelope,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "task_envelope_invalid");
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("ai_task_envelope_unsupported_version"));

    // Task still locked by the same owner: a plain fail goes through.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/fail",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-badcat",
            "error": "boom",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "plain fail still works: {resp:?}");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// worker-sdk typed round trip (E2E over real HTTP)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_sdk_complete_with_envelope_end_to_end() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, AI_CHAIN_BPMN).await;
    let instance_id = start_instance(&client, &server, "ai-chain-flow", serde_json::json!({}))
        .await
        .expect("start should succeed");

    let engine = EngineClient::new(&server.base_url);
    let worker_id = "w-sdk";

    // Step A: fetch via the SDK, build a typed TaskOutput, complete WITH the
    // envelope via complete_with_envelope (same wire format as a manual
    // variable — no new endpoint semantics).
    let tasks = engine
        .fetch_and_lock(worker_id, &["ai-a".to_string()], 1, 60_000)
        .await
        .expect("fetch ai-a");
    assert_eq!(tasks.len(), 1);
    let task_a = &tasks[0];
    let input = TaskInput::from_task_variables(&task_a.variables);
    // No envelope in the fresh input.
    assert!(input.envelope().is_none());

    let mut envelope = new_envelope(&task_a.task_id, 0);
    envelope.idempotency_key = Some("sha256:topic|gpt-4o|web_search".to_string());
    envelope.timeout_secs = Some(120);
    envelope.model = Some(bpm_engine_core::ModelRef {
        provider: "openai".to_string(),
        name: "gpt-4o".to_string(),
        version: Some("2024-08-06".to_string()),
    });
    envelope.tools = vec![bpm_engine_core::ToolRef {
        name: "web_search".to_string(),
        version: Some("2.1".to_string()),
    }];
    envelope.artifacts = vec![bpm_engine_core::ArtifactRef {
        uri: "https://storage.example.com/out/report.md".to_string(),
        sha256: Some("c".repeat(64)),
        size: Some(4096),
        media_type: Some("text/markdown".to_string()),
    }];
    envelope.output_schema = Some(serde_json::json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"]
    }));

    let output = TaskOutput::new()
        .variable("summary", "Q3 revenue grew 12%.")
        .json_variable("metrics", serde_json::json!({ "growth": 0.12 }))
        .envelope(envelope.clone());
    let variables = output.build().expect("valid envelope");
    engine
        .complete_with_envelope(&task_a.task_id, worker_id, variables, &envelope)
        .await
        .expect("complete_with_envelope");

    // Server side: the instance carries the envelope; core parses it back
    // into the same typed value (typed round trip over the wire).
    let instance = get_instance(&client, &server, &instance_id).await;
    let stored = instance["variables"]["__task_envelope"]
        .as_str()
        .expect("instance carries the envelope");
    assert_eq!(parse_stored_envelope(stored), envelope);
    let server_vars: HashMap<String, String> =
        serde_json::from_value(instance["variables"].clone()).unwrap();
    assert_eq!(
        AiTaskEnvelope::from_variables(&server_vars),
        Some(Ok(envelope.clone()))
    );

    // Step B: the CHAINED task's input variables contain the envelope A
    // produced (instance merge happens before the task snapshot) —
    // TaskInput reads it typed.
    let tasks = engine
        .fetch_and_lock(worker_id, &["ai-b".to_string()], 1, 60_000)
        .await
        .expect("fetch ai-b");
    assert_eq!(tasks.len(), 1, "chained task ai-b is fetchable");
    let task_b = &tasks[0];
    let input_b = TaskInput::from_task_variables(&task_b.variables);
    let upstream = input_b
        .envelope()
        .expect("ai-b input carries step A's envelope")
        .expect("envelope parses");
    assert_eq!(upstream, envelope);
    // Payload variables from step A are visible with typed access too.
    assert_eq!(input_b.plain("summary"), Some("Q3 revenue grew 12%."));
    assert_eq!(
        input_b.json("metrics"),
        Some(serde_json::json!({ "growth": 0.12 }))
    );

    // Step B completes without an envelope (unchanged flow) and the chain
    // finishes.
    engine
        .complete(&task_b.task_id, worker_id, HashMap::new())
        .await
        .expect("complete ai-b");
    let instance = get_instance(&client, &server, &instance_id).await;
    assert_eq!(instance["status"], "COMPLETED");

    server.stop().await;
}
