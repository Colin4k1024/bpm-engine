//! REST-level tests for approval, pause, and cancel semantics (E5-2,
//! issue #24 / ADR-008).
//!
//! The tests run with JWT auth configured (like the rbac suite: the secret
//! is fixed for the whole test PROCESS) and with
//! `engine.approval_required_task_types = ["payment_charge"]` so the
//! approval-required external task exercises the full
//! task-creation-after-approval flow:
//!
//! - approval-required arrival: token waits, a Pending record exists, NO
//!   task is fetchable (workers never see unapproved work);
//! - approve (operator): the task is created with the ENGINE-written
//!   `__approval=true`, the worker completes it, the instance completes;
//!   history explains the flow (ApprovalCreated/ApprovalGranted with the
//!   actor);
//! - reject: the token is failed (TokenFailed), the step is dead-lettered
//!   (error naming the rejecting actor), the instance terminates;
//! - only-once: a retried same decision replays 200, a conflicting
//!   decision answers 409 `approval_already_decided`, ONE task is created;
//! - decision/effect re-drive (ADR-008 §7.1): a decide whose durable
//!   decision is committed but whose effect was never applied (simulated by
//!   deciding through the STORE directly, bypassing the route) is re-driven
//!   by a same-decision replay, by the startup sweep, and idempotently
//!   (never a second task / dead-letter entry);
//! - pause: tokens Suspended, task complete refused (409 `instance_paused`),
//!   resume completes; double-pause is an idempotent replay; pause on a
//!   terminal instance is 409 `instance_not_pausable`;
//! - timers during pause are RESCHEDULED (not consumed — the deadline
//!   survives the pause and fires after resume); a catch token resumed from
//!   pause returns to Waiting (not Ready), so the catch can fire;
//! - messages/signals delivered while their catch token is Suspended are
//!   DROPPED (documented v1 semantics, ADR-008 §5): delivered-while-paused
//!   is not consumed; delivered after resume is;
//! - cancel: instance TERMINATED, task ops rejected (409
//!   `instance_cancelled`), cancel replay is 200 with exactly one
//!   InstanceCancelled history event — including under two CONCURRENT
//!   cancels; a reject racing (or following) a cancel never RESURRECTS the
//!   terminated instance (the reject effect runs under the lifecycle lock);
//! - RBAC: decide/pause/resume/cancel are operator actions; the approval
//!   list is a read; unknown ids answer 404 `approval_not_found`.
//!
//! Memory driver — runs in CI by default (no Docker). The Postgres
//! approval-store trait behavior (subject idempotency + only-once decide +
//! the decision/effect re-drivable unit) is covered by
//! `crates/adapters/postgres/tests/integration.rs::approval_store_only_once_and_subject_idempotent`
//! (Docker-gated).

use bpm_engine_core::{ApprovalDecision, ApprovalEffect};
use bpm_engine_server_rest::bpm_config::{
    AuthConfig, BpmConfig, DatabaseConfig, EngineConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::routes::recover_pending_approval_effects;
use bpm_engine_server_rest::state::AppState;
use bpm_engine_server_rest::{build_app, middleware::mint_jwt};
use bpm_engine_storage::ExternalTaskStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const TEST_SECRET: &str = "approval-test-secret";
const TENANT: &str = "acme";

/// An external task whose type is in the approval-required set.
const APPROVAL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="approval-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="charge" name="Charge">
      <extensionElements>
        <taskDefinition type="payment_charge"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="charge"/>
    <sequenceFlow id="f2" sourceRef="charge" targetRef="end"/>
  </process>
</definitions>"#;

/// An external task whose type is NOT approval-required (control: the
/// pre-E5-2 behavior — immediately fetchable).
const PLAIN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="plain-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work" name="Work">
      <extensionElements>
        <taskDefinition type="plain_work"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
    <sequenceFlow id="f2" sourceRef="work" targetRef="end"/>
  </process>
</definitions>"#;

/// A PT1S timer intermediate catch event (F3: a timer firing DURING a
/// pause must be rescheduled, never consumed — the deadline survives).
const TIMER_CATCH_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="timer-catch-approval-flow" isExecutable="true">
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

/// A message intermediate catch (F4: delivery while the catch token is
/// Suspended is DROPPED — the documented v1 semantics).
const MESSAGE_CATCH_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <message id="msg-confirmed" name="order-confirmed"/>
  <process id="message-catch-flow" isExecutable="true">
    <startEvent id="start"/>
    <intermediateCatchEvent id="await-confirmation">
      <messageEventDefinition messageRef="msg-confirmed"/>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="await-confirmation"/>
    <sequenceFlow id="f2" sourceRef="await-confirmation" targetRef="end"/>
  </process>
</definitions>"#;

/// A message intermediate throw — the delivery vehicle for the catch above.
const MESSAGE_THROW_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <message id="msg-confirmed" name="order-confirmed"/>
  <process id="message-throw-flow" isExecutable="true">
    <startEvent id="start"/>
    <intermediateThrowEvent id="emit-confirmation">
      <messageEventDefinition messageRef="msg-confirmed"/>
    </intermediateThrowEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="emit-confirmation"/>
    <sequenceFlow id="f2" sourceRef="emit-confirmation" targetRef="end"/>
  </process>
</definitions>"#;

/// A signal intermediate catch (F4, the signal variant of the same drop).
const SIGNAL_CATCH_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <signal id="sig-shipped" name="order-shipped"/>
  <process id="signal-catch-flow" isExecutable="true">
    <startEvent id="start"/>
    <intermediateCatchEvent id="await-shipping">
      <signalEventDefinition signalRef="sig-shipped"/>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="await-shipping"/>
    <sequenceFlow id="f2" sourceRef="await-shipping" targetRef="end"/>
  </process>
</definitions>"#;

/// A signal intermediate throw — the delivery vehicle for the catch above.
const SIGNAL_THROW_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <signal id="sig-shipped" name="order-shipped"/>
  <process id="signal-throw-flow" isExecutable="true">
    <startEvent id="start"/>
    <intermediateThrowEvent id="emit-shipping">
      <signalEventDefinition signalRef="sig-shipped"/>
    </intermediateThrowEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="emit-shipping"/>
    <sequenceFlow id="f2" sourceRef="emit-shipping" targetRef="end"/>
  </process>
</definitions>"#;

struct TestServer {
    base_url: String,
    /// The app state — the tests use its storage trait objects directly to
    /// simulate crash windows (e.g. deciding through the STORE, bypassing
    /// the route's effect application).
    state: Arc<AppState>,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

fn config() -> BpmConfig {
    BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        auth: AuthConfig {
            jwt_secret: TEST_SECRET.to_string(),
            api_key: String::new(),
        },
        engine: EngineConfig {
            approval_required_task_types: vec!["payment_charge".to_string()],
            // Fast timer poll so the timer-during-pause tests are quick but
            // still exercise the real scheduler + drain wiring.
            timer_poll_interval_ms: 200,
            timer_batch_size: 100,
            ..Default::default()
        },
        rate_limit: RateLimitConfig {
            requests_per_minute: 1_000_000,
        },
        ..Default::default()
    }
}

async fn spawn_server() -> TestServer {
    let app = build_app(&config()).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let timer_cancel = app.timer_cancel.clone();
    let router = app.router.clone();
    let state = Arc::clone(&app.state);
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestServer {
        base_url: format!("http://{}", addr),
        state,
        serve_task,
        timer_cancel,
    }
}

impl TestServer {
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Now + one hour (unix seconds).
fn soon() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

fn bearer(role: &str, tenant: &str, sub: &str) -> String {
    format!(
        "Bearer {}",
        mint_jwt(TEST_SECRET, sub, tenant, role, soon()).expect("mint token")
    )
}

async fn deploy(client: &reqwest::Client, server: &TestServer, bpmn: &str) -> String {
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .header("Authorization", bearer("deployer", TENANT, "deployer-1"))
        .body(bpmn.to_string())
        .send()
        .await
        .expect("deploy request");
    assert_eq!(resp.status(), 201, "deploy should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["process_definition_id"].as_str().unwrap().to_string()
}

/// Start an instance as the operator; returns the instance id.
async fn start_instance(client: &reqwest::Client, server: &TestServer, def: &str) -> String {
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .json(&serde_json::json!({ "process_def_id": def }))
        .send()
        .await
        .expect("start request");
    assert_eq!(resp.status(), 201, "start should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["instance_id"].as_str().unwrap().to_string()
}

/// Fetch and lock as the worker; returns the delivered tasks.
async fn fetch_and_lock(
    client: &reqwest::Client,
    server: &TestServer,
    task_type: &str,
) -> Vec<serde_json::Value> {
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-1"))
        .json(&serde_json::json!({
            "worker_id": "worker-1",
            "task_types": [task_type],
            "max_tasks": 10,
            "lock_duration_ms": 60_000,
        }))
        .send()
        .await
        .expect("fetch-and-lock request");
    assert_eq!(resp.status(), 200, "fetch-and-lock should return 200");
    resp.json().await.unwrap()
}

async fn get_instance(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
) -> serde_json::Value {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            server.base_url, id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("get instance");
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

async fn list_approvals(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
) -> serde_json::Value {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/approvals",
            server.base_url, id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("list approvals");
    assert_eq!(resp.status(), 200, "approval list should return 200");
    resp.json().await.unwrap()
}

/// Decide an approval as the operator; returns (status, body).
async fn decide(
    client: &reqwest::Client,
    server: &TestServer,
    approval_id: &str,
    decision: &str,
    note: Option<&str>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!(
            "{}/api/v1/approvals/{}/decide",
            server.base_url, approval_id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .json(&serde_json::json!({ "decision": decision, "note": note }))
        .send()
        .await
        .expect("decide request");
    let status = resp.status();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

async fn history(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
) -> Vec<serde_json::Value> {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            server.base_url, id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("history request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    body["events"].as_array().unwrap().clone()
}

/// Poll GET instance until `status` equals `expected` (panics after timeout).
async fn wait_for_status(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
    expected: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let inst = get_instance(client, server, id).await;
        if inst["status"].as_str() == Some(expected) {
            return inst;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "instance did not reach {expected} within {timeout:?}: {inst}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// POST a lifecycle op (pause/resume/cancel) as the operator; returns
/// (status, body).
async fn lifecycle_op(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
    op: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/{}",
            server.base_url, id, op
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("lifecycle request");
    let status = resp.status();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

/// The tenant-visible dead-letter entries (operator read).
async fn dead_letters(client: &reqwest::Client, server: &TestServer) -> Vec<serde_json::Value> {
    let resp = client
        .get(format!("{}/api/v1/dead-letters", server.base_url))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("dead letters request");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    body["entries"].as_array().unwrap().clone()
}

/// Decide a record DIRECTLY through the store, bypassing the decide route:
/// the DURABLE decision commits (status flips, effect stays Pending) while
/// the route's effect application never runs — exactly the crash window of
/// ADR-008 §7.1 (decision durable, effect pending).
async fn decide_via_store(server: &TestServer, approval_id: &str, decision: ApprovalDecision) {
    use bpm_engine_storage::ApprovalDecideOutcome;
    let outcome = server
        .state
        .storage
        .approval_store
        .decide(approval_id, decision, "crash-window", None)
        .await
        .expect("store-level decide");
    match outcome {
        ApprovalDecideOutcome::Decided(record) => {
            assert_eq!(
                record.effect,
                ApprovalEffect::Pending,
                "a store-level decide leaves the effect Pending (the route \
                 applies and confirms it)"
            );
        }
        other => panic!("expected a fresh Decided outcome, got {other:?}"),
    }
}

/// The record as the store currently holds it.
async fn approval_record(server: &TestServer, approval_id: &str) -> serde_json::Value {
    let record = server
        .state
        .storage
        .approval_store
        .get(approval_id)
        .await
        .expect("approval get")
        .expect("the approval record exists");
    serde_json::json!({
        "status": format!("{:?}", record.status),
        "effect": format!("{:?}", record.effect),
    })
}

/// Count history events of one type.
fn history_count(events: &[serde_json::Value], event_type: &str) -> usize {
    events
        .iter()
        .filter(|e| e["event_type"] == event_type)
        .count()
}

// --- Approve flow ------------------------------------------------------------

#[tokio::test]
async fn approval_required_step_waits_then_approve_unlocks_work() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;

    // The token waits; NO task is fetchable (workers never see unapproved
    // work — task-creation-after-approval).
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert!(tasks.is_empty(), "unapproved work must not be fetchable");

    // A Pending approval record exists for the step.
    let approvals = list_approvals(&client, &server, &instance).await;
    assert_eq!(approvals["approvals"].as_array().unwrap().len(), 1);
    let record = &approvals["approvals"][0];
    assert_eq!(record["status"], "PENDING");
    assert_eq!(record["task_type"], "payment_charge");
    assert_eq!(record["node_id"], "charge");
    let approval_id = record["approval_id"].as_str().unwrap().to_string();

    // Approve (operator): the task is created, with the ENGINE-written
    // __approval=true derivation.
    let (status, body) = decide(&client, &server, &approval_id, "approve", Some("ok")).await;
    assert_eq!(status, 201 - 1, "decide approve should return 200");
    assert_eq!(body["status"], "APPROVED");
    assert_eq!(body["decided_by"], "alice");

    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(tasks.len(), 1, "approved work is fetchable");
    assert_eq!(
        tasks[0]["variables"]["__approval"].as_str(),
        Some("true"),
        "the engine writes __approval=true at task creation (E3-3 gate reads engine truth)"
    );

    // The worker completes; the instance completes.
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-1"))
        .json(&serde_json::json!({
            "worker_id": "worker-1",
            "variables": { "result": "charged" },
        }))
        .send()
        .await
        .expect("complete request");
    assert_eq!(resp.status(), 200, "complete should return 200");
    wait_for_status(
        &client,
        &server,
        &instance,
        "COMPLETED",
        Duration::from_secs(5),
    )
    .await;

    // History explains the approval: created (engine), granted (operator
    // actor), and the actor is recorded.
    let events = history(&client, &server, &instance).await;
    let types: Vec<&str> = events
        .iter()
        .filter_map(|e| e["event_type"].as_str())
        .collect();
    assert!(types.contains(&"ApprovalCreated"), "events: {types:?}");
    assert!(types.contains(&"ApprovalGranted"), "events: {types:?}");
    let granted = events
        .iter()
        .find(|e| e["event_type"] == "ApprovalGranted")
        .unwrap();
    assert_eq!(granted["payload"]["actor"], "alice");
    assert_eq!(granted["payload"]["note"], "ok");

    server.stop().await;
}

#[tokio::test]
async fn ungated_task_type_is_immediately_fetchable() {
    // Control: a task type NOT in the approval-required set keeps the
    // pre-E5-2 behavior — no record, immediately fetchable.
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, PLAIN_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;

    let tasks = fetch_and_lock(&client, &server, "plain_work").await;
    assert_eq!(tasks.len(), 1, "ungated task is immediately fetchable");
    assert!(!tasks[0]["variables"]
        .as_object()
        .unwrap()
        .contains_key("__approval"));
    let approvals = list_approvals(&client, &server, &instance).await;
    assert_eq!(approvals["approvals"].as_array().unwrap().len(), 0);

    server.stop().await;
}

#[tokio::test]
async fn decide_errors_are_typed() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let _def = deploy(&client, &server, APPROVAL_BPMN).await;

    // Unknown approval id → 404 approval_not_found.
    let (status, body) = decide(
        &client,
        &server,
        "00000000-0000-4000-8000-000000000000",
        "approve",
        None,
    )
    .await;
    assert_eq!(status.as_u16(), 404);
    assert_eq!(body["code"], "approval_not_found");

    // Invalid decision string → 400 invalid_request.
    let instance = start_instance(&client, &server, &_def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = decide(&client, &server, &approval_id, "maybe", None).await;
    assert_eq!(status.as_u16(), 400);
    assert_eq!(body["code"], "invalid_request");

    // RBAC: the worker role may not decide (403 forbidden).
    let resp = client
        .post(format!(
            "{}/api/v1/approvals/{}/decide",
            server.base_url, approval_id
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-1"))
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "forbidden");

    server.stop().await;
}

#[tokio::test]
async fn cross_tenant_approval_is_not_found() {
    // E5-1 tenancy + E5-2: another tenant's approval is reported exactly
    // like an unknown id (no existence leak).
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(format!(
            "{}/api/v1/approvals/{}/decide",
            server.base_url, approval_id
        ))
        .header("Authorization", bearer("operator", "globex", "mallory"))
        .json(&serde_json::json!({ "decision": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "approval_not_found");

    // The list of another tenant's instance is 404 too.
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/approvals",
            server.base_url, instance
        ))
        .header("Authorization", bearer("operator", "globex", "mallory"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);

    server.stop().await;
}

// --- Only-once semantics -----------------------------------------------------

#[tokio::test]
async fn double_decide_is_only_once() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    // First decide wins.
    let (status, body) = decide(&client, &server, &approval_id, "approve", Some("first")).await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["status"], "APPROVED");
    assert_eq!(body["decided_by"], "alice");
    assert_eq!(body["note"], "first");

    // Retried SAME decision: 200 replay with the recorded outcome.
    let (status, body) = decide(&client, &server, &approval_id, "approve", Some("retry")).await;
    assert_eq!(status.as_u16(), 200, "same-decision retry is a replay");
    assert_eq!(body["status"], "APPROVED");
    assert_eq!(
        body["note"], "first",
        "the retry's note must not overwrite the original decision"
    );

    // CONFLICTING decision: 409 approval_already_decided; the record keeps
    // the original outcome.
    let (status, body) = decide(&client, &server, &approval_id, "reject", None).await;
    assert_eq!(status.as_u16(), 409);
    assert_eq!(body["code"], "approval_already_decided");
    let approvals = list_approvals(&client, &server, &instance).await;
    assert_eq!(approvals["approvals"][0]["status"], "APPROVED");

    // ONE task exists despite the extra decides (only-once effect).
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(tasks.len(), 1, "exactly one task created");

    // History: exactly ONE ApprovalGranted (the replay/conflict appended
    // nothing).
    let events = history(&client, &server, &instance).await;
    let granted = events
        .iter()
        .filter(|e| e["event_type"] == "ApprovalGranted")
        .count();
    assert_eq!(
        granted, 1,
        "one ApprovalGranted event despite double decide"
    );

    server.stop().await;
}

#[tokio::test]
async fn concurrent_decides_produce_one_effect() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Two CONCURRENT approve decides: both answer 200 (one decides, one
    // replays the same outcome), and exactly one task is created.
    let url = format!(
        "{}/api/v1/approvals/{}/decide",
        server.base_url, approval_id
    );
    let (a, b) = tokio::join!(
        client
            .post(&url)
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .json(&serde_json::json!({ "decision": "approve" }))
            .send(),
        client
            .post(&url)
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .json(&serde_json::json!({ "decision": "approve" }))
            .send(),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.status().as_u16(), 200, "concurrent decide a");
    assert_eq!(b.status().as_u16(), 200, "concurrent decide b");
    let body_a: serde_json::Value = a.json().await.unwrap();
    let body_b: serde_json::Value = b.json().await.unwrap();
    assert_eq!(body_a["status"], "APPROVED");
    assert_eq!(body_b["status"], "APPROVED");

    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(tasks.len(), 1, "one task created under concurrent decides");

    server.stop().await;
}

// --- Reject flow -------------------------------------------------------------

#[tokio::test]
async fn reject_dead_letters_and_terminates_the_instance() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = decide(&client, &server, &approval_id, "reject", Some("too risky")).await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["status"], "REJECTED");

    // No task is ever created for a rejected step.
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert!(tasks.is_empty());

    // The rejected step's TOKEN is terminated (operator-forced transition),
    // while the INSTANCE keeps running — exactly the E3-2 permanent-failure
    // pattern: a dead step leaves the instance to its remaining branches
    // and the DLQ entry is the operator's lever.
    let inst = get_instance(&client, &server, &instance).await;
    assert_eq!(inst["status"], "RUNNING");
    let charge = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "charge")
        .expect("the charge token");
    assert_eq!(charge["status"], "Terminated", "rejected token: {charge}");

    // The step is dead-lettered with the rejecting actor in the message.
    let resp = client
        .get(format!("{}/api/v1/dead-letters", server.base_url))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let dl: serde_json::Value = resp.json().await.unwrap();
    let entries = dl["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "the rejected step is dead-lettered");
    assert_eq!(entries[0]["task_type"], "payment_charge");
    let msg = entries[0]["error_message"].as_str().unwrap();
    assert!(
        msg.contains("approval rejected by alice"),
        "the DLQ message names the rejecting actor: {msg}"
    );

    // History explains the rejection: ApprovalRejected (actor), TokenFailed.
    let events = history(&client, &server, &instance).await;
    let types: Vec<&str> = events
        .iter()
        .filter_map(|e| e["event_type"].as_str())
        .collect();
    assert!(types.contains(&"ApprovalRejected"), "events: {types:?}");
    assert!(types.contains(&"TokenFailed"), "events: {types:?}");
    let rejected = events
        .iter()
        .find(|e| e["event_type"] == "ApprovalRejected")
        .unwrap();
    assert_eq!(rejected["payload"]["actor"], "alice");
    assert_eq!(rejected["payload"]["note"], "too risky");

    server.stop().await;
}

// --- Pause / resume ----------------------------------------------------------

#[tokio::test]
async fn pause_freezes_transitions_and_resume_completes() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;

    // Approve and deliver the task, then lock it as the worker.
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    decide(&client, &server, &approval_id, "approve", None).await;
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(tasks.len(), 1);
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();

    // Pause (operator): tokens → Suspended.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/pause",
            server.base_url, instance
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "RUNNING");
    assert_eq!(body["paused"], true, "the instance state stays RUNNING");
    let inst = get_instance(&client, &server, &instance).await;
    assert!(
        inst["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["status"] == "Suspended"),
        "tokens are Suspended: {inst}"
    );

    // Double pause: idempotent 200 replay, no second history event.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/pause",
            server.base_url, instance
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let events = history(&client, &server, &instance).await;
    let paused_events = events
        .iter()
        .filter(|e| e["event_type"] == "InstancePaused")
        .count();
    assert_eq!(
        paused_events, 1,
        "one InstancePaused event for double pause"
    );
    let paused = events
        .iter()
        .find(|e| e["event_type"] == "InstancePaused")
        .unwrap();
    assert_eq!(paused["payload"]["actor"], "alice");

    // Task completion is refused while paused (409 instance_paused); the
    // task stays locked.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-1"))
        .json(&serde_json::json!({ "worker_id": "worker-1", "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "instance_paused");

    // Lease maintenance (extend-lock) is still allowed during the pause.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/extend-lock",
            server.base_url, task_id
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-1"))
        .json(&serde_json::json!({
            "worker_id": "worker-1",
            "extension_ms": 30_000,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "extend-lock works while paused"
    );

    // Resume: tokens flip Suspended → Ready; the completion proceeds and
    // the instance completes.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/resume",
            server.base_url, instance
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["paused"], false);

    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-1"))
        .json(&serde_json::json!({
            "worker_id": "worker-1",
            "variables": { "result": "ok" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "complete after resume");
    wait_for_status(
        &client,
        &server,
        &instance,
        "COMPLETED",
        Duration::from_secs(5),
    )
    .await;

    let events = history(&client, &server, &instance).await;
    let types: Vec<&str> = events
        .iter()
        .filter_map(|e| e["event_type"].as_str())
        .collect();
    assert!(types.contains(&"InstancePaused"), "events: {types:?}");
    assert!(types.contains(&"InstanceResumed"), "events: {types:?}");
    let resumed = events
        .iter()
        .find(|e| e["event_type"] == "InstanceResumed")
        .unwrap();
    assert_eq!(resumed["payload"]["actor"], "alice");

    // Pause on the terminal instance: 409 instance_not_pausable.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/pause",
            server.base_url, instance
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "instance_not_pausable");

    // RBAC: pause is an operator action.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/pause",
            server.base_url, instance
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "forbidden");

    server.stop().await;
}

// --- Cancel ------------------------------------------------------------------

#[tokio::test]
async fn cancel_terminates_and_rejects_task_ops() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;

    // Approve, deliver, and lock the task, then cancel.
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    decide(&client, &server, &approval_id, "approve", None).await;
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(tasks.len(), 1);
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/cancel",
            server.base_url, instance
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "TERMINATED");

    let inst = get_instance(&client, &server, &instance).await;
    assert_eq!(inst["status"], "TERMINATED");
    assert!(
        inst["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] == "Terminated"),
        "tokens are Terminated: {inst}"
    );

    // Task operations on the cancelled instance are rejected (409).
    // External-task ops act as the worker (the lease owner), the user-task
    // completion acts as the operator (its role).
    for (path, body_json, role) in [
        (
            format!("/api/v1/external-tasks/{}/complete", task_id),
            serde_json::json!({ "worker_id": "worker-1", "variables": {} }),
            "worker",
        ),
        (
            format!("/api/v1/external-tasks/{}/fail", task_id),
            serde_json::json!({ "worker_id": "worker-1", "error": "boom" }),
            "worker",
        ),
        (
            format!("/api/v1/external-tasks/{}/extend-lock", task_id),
            serde_json::json!({ "worker_id": "worker-1", "extension_ms": 1000 }),
            "worker",
        ),
        // A user-task completion is equally rejected (the task id has the
        // <instance>:<node> user-task shape).
        (
            format!("/api/v1/tasks/{}:charge/complete", instance),
            serde_json::json!({ "variables": {} }),
            "operator",
        ),
    ] {
        let resp = client
            .post(format!("{}{}", server.base_url, path))
            .header("Authorization", bearer(role, TENANT, "actor-1"))
            .json(&body_json)
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        assert_eq!(
            status, 409,
            "op {path} should be rejected on a cancelled instance: {body}"
        );
        assert_eq!(
            body["code"], "instance_cancelled",
            "op {path} rejected with the stable code: {body}"
        );
    }

    // Only-once: the second cancel replays 200 with the terminal state and
    // does NOT append a second InstanceCancelled.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/cancel",
            server.base_url, instance
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let events = history(&client, &server, &instance).await;
    let cancelled = events
        .iter()
        .filter(|e| e["event_type"] == "InstanceCancelled")
        .count();
    assert_eq!(
        cancelled, 1,
        "one InstanceCancelled event for double cancel"
    );
    let entry = events
        .iter()
        .find(|e| e["event_type"] == "InstanceCancelled")
        .unwrap();
    assert_eq!(entry["payload"]["actor"], "alice");
    assert!(
        events
            .iter()
            .any(|e| e["event_type"] == "ProcessTerminated"),
        "the TerminateEnd-family engine event is recorded"
    );

    server.stop().await;
}

#[tokio::test]
async fn concurrent_cancels_produce_one_effect() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    decide(&client, &server, &approval_id, "approve", None).await;

    // Two CONCURRENT cancels: both answer 200, the instance is terminated
    // once, and exactly ONE InstanceCancelled history event exists.
    let url = format!(
        "{}/api/v1/process-instances/{}/cancel",
        server.base_url, instance
    );
    let (a, b) = tokio::join!(
        client
            .post(&url)
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .send(),
        client
            .post(&url)
            .header("Authorization", bearer("operator", TENANT, "bob"))
            .send(),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(
        a.status().as_u16(),
        200,
        "concurrent cancel a: {}",
        a.status()
    );
    assert_eq!(
        b.status().as_u16(),
        200,
        "concurrent cancel b: {}",
        b.status()
    );

    let inst = get_instance(&client, &server, &instance).await;
    assert_eq!(inst["status"], "TERMINATED");
    let events = history(&client, &server, &instance).await;
    let cancelled = events
        .iter()
        .filter(|e| e["event_type"] == "InstanceCancelled")
        .count();
    assert_eq!(
        cancelled, 1,
        "concurrent cancels must produce exactly one effect"
    );
    let terminated = events
        .iter()
        .filter(|e| e["event_type"] == "ProcessTerminated")
        .count();
    assert_eq!(terminated, 1, "one ProcessTerminated event");

    server.stop().await;
}

#[tokio::test]
async fn cancel_and_pause_on_unknown_instance_are_404() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let missing = "00000000-0000-4000-8000-000000000000";
    for op in ["pause", "resume", "cancel"] {
        let resp = client
            .post(format!(
                "{}/api/v1/process-instances/{}/{}",
                server.base_url, missing, op
            ))
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404, "{op} on unknown instance");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"], "not_found");
    }
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/approvals",
            server.base_url, missing
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);

    server.stop().await;
}

// --- Decision/effect re-drive (F1, ADR-008 §7.1) -----------------------------
//
// The decision and its effect are a RE-DRIVABLE unit: `decide` commits the
// decision durably with effect=Pending; the effect application (task
// creation on grant, dead letter + token-terminal on reject) confirms it.
// A crash between the two is simulated by deciding through the STORE
// directly (the route's effect application never runs), then converging via
// the two re-drive paths: a retried same-decision decide, and the startup
// sweep.

#[tokio::test]
async fn decide_replay_re_drives_a_pending_grant_effect() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Simulated crash: the decision committed (durable), the effect never
    // ran — no task exists.
    decide_via_store(&server, &approval_id, ApprovalDecision::Approve).await;
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert!(
        tasks.is_empty(),
        "the simulated crash leaves the grant un-applied (no task)"
    );

    // A retried SAME decision: 200 replay AND the pending effect is
    // re-driven BEFORE answering (self-healing retry).
    let (status, body) = decide(&client, &server, &approval_id, "approve", None).await;
    assert_eq!(status.as_u16(), 200, "same-decision retry is a replay");
    assert_eq!(body["status"], "APPROVED");
    assert_eq!(
        body["decided_by"], "crash-window",
        "the replay answers with the recorded (crash-window) decision"
    );

    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(tasks.len(), 1, "the re-drive created the task");
    assert_eq!(tasks[0]["variables"]["__approval"].as_str(), Some("true"));
    let record = approval_record(&server, &approval_id).await;
    assert_eq!(record["effect"], "Applied", "the effect is confirmed");

    server.stop().await;
}

#[tokio::test]
async fn pending_grant_effect_re_drive_never_duplicates_the_task() {
    // Crash AFTER the task insert but BEFORE mark_effect_applied: the task
    // exists while the effect is still Pending. The re-drive must detect
    // the existing task (idempotent) and NOT create a second one.
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    let record = server
        .state
        .storage
        .approval_store
        .get(&approval_id)
        .await
        .unwrap()
        .unwrap();

    // Simulated partial application: durable decision + task already
    // created, effect still Pending.
    decide_via_store(&server, &approval_id, ApprovalDecision::Approve).await;
    let mut variables = HashMap::new();
    variables.insert("__approval".to_string(), "true".to_string());
    ExternalTaskStore::create(
        &*server.state.storage.external_task_store,
        &record.token_id,
        &record.instance_id,
        "payment_charge",
        3,
        60,
        variables,
    )
    .await
    .unwrap();

    // The retried decide re-drives: it finds the existing task, confirms
    // the effect, and creates NO second task.
    let (status, _) = decide(&client, &server, &approval_id, "approve", None).await;
    assert_eq!(status.as_u16(), 200);
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(
        tasks.len(),
        1,
        "the idempotent re-drive must not duplicate the task"
    );
    let record = approval_record(&server, &approval_id).await;
    assert_eq!(record["effect"], "Applied");

    server.stop().await;
}

#[tokio::test]
async fn decide_replay_re_drives_a_pending_reject_effect() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Simulated crash: the rejection committed, its effect never ran.
    decide_via_store(&server, &approval_id, ApprovalDecision::Reject).await;
    assert!(
        dead_letters(&client, &server).await.is_empty(),
        "the simulated crash leaves the reject un-applied (no dead letter)"
    );

    // The retried same decision re-drives the reject effect.
    let (status, body) = decide(&client, &server, &approval_id, "reject", None).await;
    assert_eq!(status.as_u16(), 200, "same-decision retry is a replay");
    assert_eq!(body["status"], "REJECTED");

    // The rejection is recorded exactly once: one dead letter, one
    // ApprovalRejected (the re-drive appends no second history event), and
    // the token is terminated while the instance keeps running (E3-2).
    let entries = dead_letters(&client, &server).await;
    assert_eq!(entries.len(), 1, "the re-drive dead-lettered the step");
    assert!(entries[0]["error_message"]
        .as_str()
        .unwrap()
        .contains("approval rejected by crash-window"));
    let inst = get_instance(&client, &server, &instance).await;
    assert_eq!(inst["status"], "RUNNING");
    let charge = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "charge")
        .expect("the charge token");
    assert_eq!(charge["status"], "Terminated", "rejected token: {charge}");
    let events = history(&client, &server, &instance).await;
    assert_eq!(history_count(&events, "ApprovalRejected"), 1);
    assert_eq!(
        history_count(&events, "TokenFailed"),
        1,
        "the re-drive performed the token termination (exactly one)"
    );
    let record = approval_record(&server, &approval_id).await;
    assert_eq!(record["effect"], "Applied");

    server.stop().await;
}

#[tokio::test]
async fn startup_sweep_re_drives_pending_effects() {
    // The second re-drive path: the server's startup sweep (build_app
    // wires it after storage wiring; meaningful for the postgres driver —
    // the memory store starts empty). Driven directly here over a live app
    // state with two crash-window records: one grant, one reject.
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;

    // Grant side: durable decision, no task.
    let granted = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &granted).await;
    let granted_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    decide_via_store(&server, &granted_id, ApprovalDecision::Approve).await;

    // Reject side: durable decision, no dead letter.
    let rejected = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &rejected).await;
    let rejected_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    decide_via_store(&server, &rejected_id, ApprovalDecision::Reject).await;

    // The sweep re-drives both.
    recover_pending_approval_effects(&server.state).await;

    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert_eq!(
        tasks.len(),
        1,
        "the sweep created the granted task (for {granted})"
    );
    assert_eq!(
        tasks[0]["process_instance_id"].as_str(),
        Some(granted.as_str())
    );
    let record = approval_record(&server, &granted_id).await;
    assert_eq!(record["effect"], "Applied");

    let entries = dead_letters(&client, &server).await;
    assert_eq!(
        entries.len(),
        1,
        "the sweep dead-lettered the rejected step"
    );
    assert_eq!(
        entries[0]["process_instance_id"].as_str(),
        Some(rejected.as_str())
    );
    let record = approval_record(&server, &rejected_id).await;
    assert_eq!(record["effect"], "Applied");
    let inst = get_instance(&client, &server, &rejected).await;
    let charge = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "charge")
        .expect("the charge token");
    assert_eq!(charge["status"], "Terminated", "rejected token: {charge}");

    // The sweep is idempotent: a second run finds nothing pending.
    recover_pending_approval_effects(&server.state).await;
    let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
    assert!(
        tasks.is_empty(),
        "the task above is already locked; no new work appears"
    );
    assert_eq!(
        dead_letters(&client, &server).await.len(),
        1,
        "no second dead letter from the second sweep"
    );

    server.stop().await;
}

// --- Reject vs. cancel race (F2) ----------------------------------------------
//
// The reject effect runs under the instance lifecycle lock with a FRESH
// load inside it: a cancel that terminated the instance can never be
// overwritten back to Running by the reject's later save.

#[tokio::test]
async fn reject_after_cancel_never_resurrects_the_instance() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Cancel FIRST (deterministic order): the instance is Terminated.
    let (status, body) = lifecycle_op(&client, &server, &instance, "cancel").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["status"], "TERMINATED");
    let before = get_instance(&client, &server, &instance).await;
    let token_before = before["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "charge")
        .unwrap()
        .clone();

    // The late reject: 200 (the decision is recorded), and the effect is
    // applied-with-no-instance-mutation — the terminated instance stays
    // terminated (never resurrected to Running).
    let (status, body) = decide(&client, &server, &approval_id, "reject", Some("late")).await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["status"], "REJECTED");

    let inst = get_instance(&client, &server, &instance).await;
    assert_eq!(
        inst["status"], "TERMINATED",
        "the reject effect must never resurrect a cancelled instance: {inst}"
    );
    assert_eq!(inst["version"], before["version"]);
    let token_after = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "charge")
        .unwrap()
        .clone();
    assert_eq!(
        token_after["version"], token_before["version"],
        "the token is terminated exactly once (cancel's transition; the reject adds none)"
    );
    assert_eq!(token_after["status"], "Terminated");

    // The rejection is still recorded: one dead letter + the approval
    // history entry; exactly one token-terminal effect (no second
    // ProcessTerminated/TokenFailed).
    let entries = dead_letters(&client, &server).await;
    assert_eq!(entries.len(), 1, "the rejection is recorded (dead letter)");
    assert!(entries[0]["error_message"]
        .as_str()
        .unwrap()
        .contains("approval rejected by alice"));
    let events = history(&client, &server, &instance).await;
    assert_eq!(history_count(&events, "ApprovalRejected"), 1);
    assert_eq!(history_count(&events, "InstanceCancelled"), 1);
    assert_eq!(
        history_count(&events, "ProcessTerminated"),
        1,
        "exactly one instance-terminal effect"
    );
    assert_eq!(
        history_count(&events, "TokenFailed"),
        0,
        "cancel already terminated the token; the late reject adds no second \
         token-terminal effect"
    );
    let record = approval_record(&server, &approval_id).await;
    assert_eq!(record["effect"], "Applied", "the effect is complete");

    server.stop().await;
}

#[tokio::test]
async fn concurrent_cancel_and_reject_terminate_once() {
    // The race itself (order not deterministic): whichever wins the
    // lifecycle lock first, the instance ends Terminated, the rejection is
    // recorded exactly once, and the token is terminated exactly once.
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, APPROVAL_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    let approvals = list_approvals(&client, &server, &instance).await;
    let approval_id = approvals["approvals"][0]["approval_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (cancel, reject) = tokio::join!(
        client
            .post(format!(
                "{}/api/v1/process-instances/{}/cancel",
                server.base_url, instance
            ))
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .send(),
        client
            .post(format!(
                "{}/api/v1/approvals/{}/decide",
                server.base_url, approval_id
            ))
            .header("Authorization", bearer("operator", TENANT, "bob"))
            .json(&serde_json::json!({ "decision": "reject" }))
            .send(),
    );
    let cancel = cancel.unwrap();
    let reject = reject.unwrap();
    assert_eq!(cancel.status().as_u16(), 200, "concurrent cancel");
    assert_eq!(reject.status().as_u16(), 200, "concurrent reject decide");

    let inst = get_instance(&client, &server, &instance).await;
    assert_eq!(
        inst["status"], "TERMINATED",
        "the instance ends Terminated regardless of lock order: {inst}"
    );
    let charge = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "charge")
        .expect("the charge token");
    assert_eq!(charge["status"], "Terminated", "rejected token: {charge}");

    // The rejection is recorded exactly once.
    let entries = dead_letters(&client, &server).await;
    assert_eq!(entries.len(), 1, "one dead letter for the rejection");
    let events = history(&client, &server, &instance).await;
    assert_eq!(history_count(&events, "ApprovalRejected"), 1);
    assert_eq!(history_count(&events, "InstanceCancelled"), 1);
    assert_eq!(history_count(&events, "ProcessTerminated"), 1);
    // Order-dependent (whoever holds the lifecycle lock first performs the
    // token termination — the other observes it done) but NEVER more than
    // one token-terminal effect:
    assert!(history_count(&events, "TokenFailed") <= 1);
    let record = approval_record(&server, &approval_id).await;
    assert_eq!(record["effect"], "Applied", "the effect is complete");

    server.stop().await;
}

// --- Timer during pause (F3) ---------------------------------------------------

#[tokio::test]
async fn timer_catch_fires_during_pause_is_rescheduled_then_completes() {
    // A PT1S timer fires while the instance is PAUSED: the fire is NOT
    // consumed — the timer record is rescheduled until resume restores the
    // catch token to Waiting, and the deadline then advances the instance
    // to completion (the deadline SURVIVES the pause).
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, TIMER_CATCH_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;

    // The catch token waits at the timer node.
    let inst = get_instance(&client, &server, &instance).await;
    let token = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "wait-1s")
        .expect("the catch token");
    assert_eq!(token["status"], "Waiting", "catch token waits: {token}");

    // Pause; the timer's 1s deadline passes DURING the pause.
    let (status, body) = lifecycle_op(&client, &server, &instance, "pause").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["paused"], true);
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // The fire during the pause was rescheduled (not consumed): the token
    // is still Suspended and the instance is still RUNNING.
    let inst = get_instance(&client, &server, &instance).await;
    assert_eq!(inst["status"], "RUNNING", "not completed while paused");
    let token = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "wait-1s")
        .expect("the catch token");
    assert_eq!(
        token["status"], "Suspended",
        "the paused catch token is not advanced by the during-pause fire"
    );

    // Resume restores the catch token to WAITING (its pre-pause state —
    // a Ready catch token could never fire).
    let (status, body) = lifecycle_op(&client, &server, &instance, "resume").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body["paused"], false);
    let inst = get_instance(&client, &server, &instance).await;
    let token = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "wait-1s")
        .cloned();
    if let Some(token) = token {
        assert!(
            token["status"] == "Waiting",
            "resume restores the catch token to Waiting (not Ready/Suspended): {token}"
        );
    }

    // The rescheduled timer re-fires after the resume and the instance
    // completes — the deadline survived the pause.
    wait_for_status(
        &client,
        &server,
        &instance,
        "COMPLETED",
        Duration::from_secs(5),
    )
    .await;
    let events = history(&client, &server, &instance).await;
    assert!(history_count(&events, "InstancePaused") == 1);
    assert!(history_count(&events, "InstanceResumed") == 1);

    server.stop().await;
}

#[tokio::test]
async fn timer_catch_without_pause_completes_control() {
    // Control for the pause test: the same flow without pausing completes.
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def = deploy(&client, &server, TIMER_CATCH_BPMN).await;
    let instance = start_instance(&client, &server, &def).await;
    wait_for_status(
        &client,
        &server,
        &instance,
        "COMPLETED",
        Duration::from_secs(5),
    )
    .await;

    server.stop().await;
}

// --- Message/signal delivery during pause (F4) ---------------------------------
//
// Documented v1 semantics (ADR-008 §5): a message/signal delivered while
// its catch token is Suspended is DROPPED — a transient event with no
// buffer in v1. The sender retries (another throw), or the operator
// resumes first. A buffer is a v2 item.

#[tokio::test]
async fn message_delivered_while_paused_is_dropped() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let catch_def = deploy(&client, &server, MESSAGE_CATCH_BPMN).await;
    let throw_def = deploy(&client, &server, MESSAGE_THROW_BPMN).await;

    // Instance A waits at the message catch.
    let a = start_instance(&client, &server, &catch_def).await;
    wait_for_token(&client, &server, &a, "await-confirmation", "Waiting").await;

    // Pause A, then deliver the message (instance B throws it).
    let (status, _) = lifecycle_op(&client, &server, &a, "pause").await;
    assert_eq!(status.as_u16(), 200);
    let b = start_instance(&client, &server, &throw_def).await;
    wait_for_status(&client, &server, &b, "COMPLETED", Duration::from_secs(5)).await;

    // The delivery during the pause was DROPPED: A's catch token is still
    // Suspended, the instance is not completed.
    let inst = get_instance(&client, &server, &a).await;
    assert_eq!(inst["status"], "RUNNING");
    let token = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "await-confirmation")
        .expect("the catch token");
    assert_eq!(
        token["status"], "Suspended",
        "a message delivered while its catch token is Suspended is dropped (v1): {token}"
    );

    // After resume the catch token is Waiting again; a NEW delivery is
    // consumed and A completes.
    let (status, _) = lifecycle_op(&client, &server, &a, "resume").await;
    assert_eq!(status.as_u16(), 200);
    let b2 = start_instance(&client, &server, &throw_def).await;
    wait_for_status(&client, &server, &b2, "COMPLETED", Duration::from_secs(5)).await;
    wait_for_status(&client, &server, &a, "COMPLETED", Duration::from_secs(5)).await;

    server.stop().await;
}

#[tokio::test]
async fn signal_delivered_while_paused_is_dropped() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let catch_def = deploy(&client, &server, SIGNAL_CATCH_BPMN).await;
    let throw_def = deploy(&client, &server, SIGNAL_THROW_BPMN).await;

    // Instance A waits at the signal catch.
    let a = start_instance(&client, &server, &catch_def).await;
    wait_for_token(&client, &server, &a, "await-shipping", "Waiting").await;

    // Pause A, then deliver the signal (instance B throws it).
    let (status, _) = lifecycle_op(&client, &server, &a, "pause").await;
    assert_eq!(status.as_u16(), 200);
    let b = start_instance(&client, &server, &throw_def).await;
    wait_for_status(&client, &server, &b, "COMPLETED", Duration::from_secs(5)).await;

    // The delivery during the pause was DROPPED.
    let inst = get_instance(&client, &server, &a).await;
    assert_eq!(inst["status"], "RUNNING");
    let token = inst["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["node_id"] == "await-shipping")
        .expect("the catch token");
    assert_eq!(
        token["status"], "Suspended",
        "a signal delivered while its catch token is Suspended is dropped (v1): {token}"
    );

    // After resume, a new delivery is consumed and A completes.
    let (status, _) = lifecycle_op(&client, &server, &a, "resume").await;
    assert_eq!(status.as_u16(), 200);
    let b2 = start_instance(&client, &server, &throw_def).await;
    wait_for_status(&client, &server, &b2, "COMPLETED", Duration::from_secs(5)).await;
    wait_for_status(&client, &server, &a, "COMPLETED", Duration::from_secs(5)).await;

    server.stop().await;
}

/// Poll GET instance until the token at `node_id` has status `expected`
/// (panics after a short timeout — used right after start, while the
/// engine settles the token at its first wait node).
async fn wait_for_token(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
    node_id: &str,
    expected: &str,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let inst = get_instance(client, server, id).await;
        if let Some(token) = inst["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["node_id"] == node_id)
        {
            if token["status"].as_str() == Some(expected) {
                return token.clone();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "token at {node_id} did not reach {expected} within 5s: {inst}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// --- Docker (Postgres) variant ------------------------------------------------
//
// The same core approve + pause/cancel flows against a real PostgreSQL
// server (the store-trait-level Postgres coverage lives in
// crates/adapters/postgres/tests/integration.rs; this variant exercises the
// full REST path over the Postgres bundle, like scheduler_workers_postgres).
//
// Requires Docker. Run with:
//
// ```text
// cargo test -p bpm-engine-server-rest --test approval_semantics -- --ignored
// ```

#[cfg(feature = "postgres")]
mod postgres_mode {
    use super::*;

    /// Start a throwaway PostgreSQL container and build a JWT + approval
    /// config for it. Waits until the container accepts queries (postgres
    /// accepts TCP before finishing startup; `build_app` runs migrations on
    /// the first pool connection).
    async fn postgres_server_config() -> BpmConfig {
        use testcontainers::clients;
        use testcontainers_modules::postgres::Postgres;

        let docker: &'static clients::Cli = Box::leak(Box::new(clients::Cli::default()));
        let container = docker.run(Postgres::default());
        // Keep the container alive for the whole test process (dropping a
        // testcontainers Container removes the database).
        let container: &'static testcontainers::Container<'static, Postgres> =
            Box::leak(Box::new(container));
        let port = container.get_host_port_ipv4(5432);
        let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match probe_postgres(&url).await {
                Ok(()) => break,
                Err(e) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let _ = e;
                }
                Err(e) => panic!("postgres container not ready within 30s: {}", e),
            }
        }

        BpmConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
            },
            database: DatabaseConfig {
                driver: "postgres".to_string(),
                url,
                max_connections: 5,
                migrate_on_start: true,
            },
            auth: AuthConfig {
                jwt_secret: TEST_SECRET.to_string(),
                api_key: String::new(),
            },
            engine: EngineConfig {
                approval_required_task_types: vec!["payment_charge".to_string()],
                ..Default::default()
            },
            rate_limit: RateLimitConfig {
                requests_per_minute: 1_000_000,
            },
            ..Default::default()
        }
    }

    /// Open a connection and run a trivial query to verify postgres serves.
    async fn probe_postgres(url: &str) -> anyhow::Result<()> {
        let pool = bpm_engine_adapter_postgres::create_pool(url)?;
        let client = pool.get().await?;
        client.execute("SELECT 1", &[]).await?;
        Ok(())
    }

    async fn spawn_server_with(config: BpmConfig) -> TestServer {
        let app = build_app(&config).await.expect("build_app failed");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let timer_cancel = app.timer_cancel.clone();
        let router = app.router.clone();
        let state = Arc::clone(&app.state);
        let serve_task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        TestServer {
            base_url: format!("http://{}", addr),
            state,
            serve_task,
            timer_cancel,
        }
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn approval_flow_over_postgres() {
        let server = spawn_server_with(postgres_server_config().await).await;
        let client = reqwest::Client::new();
        let def = deploy(&client, &server, APPROVAL_BPMN).await;
        let instance = start_instance(&client, &server, &def).await;

        // Unapproved work is not fetchable; the Pending record exists.
        let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
        assert!(tasks.is_empty());
        let approvals = list_approvals(&client, &server, &instance).await;
        let approval_id = approvals["approvals"][0]["approval_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(approvals["approvals"][0]["status"], "PENDING");

        // Approve → the task is created with the engine-written __approval.
        let (status, body) = decide(&client, &server, &approval_id, "approve", Some("ok")).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(body["status"], "APPROVED");
        let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["variables"]["__approval"], "true");

        // Only-once over the conditional UPDATE: a conflicting decide is 409.
        let (status, body) = decide(&client, &server, &approval_id, "reject", None).await;
        assert_eq!(status.as_u16(), 409);
        assert_eq!(body["code"], "approval_already_decided");

        // Pause → complete refused → resume → complete → instance completes.
        let resp = client
            .post(format!(
                "{}/api/v1/process-instances/{}/pause",
                server.base_url, instance
            ))
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();
        let resp = client
            .post(format!(
                "{}/api/v1/external-tasks/{}/complete",
                server.base_url, task_id
            ))
            .header("Authorization", bearer("worker", TENANT, "worker-1"))
            .json(&serde_json::json!({ "worker_id": "worker-1", "variables": {} }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 409);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"], "instance_paused");

        let resp = client
            .post(format!(
                "{}/api/v1/process-instances/{}/resume",
                server.base_url, instance
            ))
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let resp = client
            .post(format!(
                "{}/api/v1/external-tasks/{}/complete",
                server.base_url, task_id
            ))
            .header("Authorization", bearer("worker", TENANT, "worker-1"))
            .json(&serde_json::json!({ "worker_id": "worker-1", "variables": {} }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        wait_for_status(
            &client,
            &server,
            &instance,
            "COMPLETED",
            Duration::from_secs(10),
        )
        .await;

        server.stop().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn reject_and_cancel_over_postgres() {
        let server = spawn_server_with(postgres_server_config().await).await;
        let client = reqwest::Client::new();
        let def = deploy(&client, &server, APPROVAL_BPMN).await;

        // Reject: dead letter + terminated token.
        let instance = start_instance(&client, &server, &def).await;
        let approvals = list_approvals(&client, &server, &instance).await;
        let approval_id = approvals["approvals"][0]["approval_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (status, _) = decide(&client, &server, &approval_id, "reject", Some("no")).await;
        assert_eq!(status.as_u16(), 200);
        assert!(fetch_and_lock(&client, &server, "payment_charge")
            .await
            .is_empty());
        let inst = get_instance(&client, &server, &instance).await;
        let charge = inst["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["node_id"] == "charge")
            .unwrap();
        assert_eq!(charge["status"], "Terminated");

        // Cancel: task ops rejected afterwards.
        let instance2 = start_instance(&client, &server, &def).await;
        let approvals2 = list_approvals(&client, &server, &instance2).await;
        let approval_id2 = approvals2["approvals"][0]["approval_id"]
            .as_str()
            .unwrap()
            .to_string();
        decide(&client, &server, &approval_id2, "approve", None).await;
        let tasks = fetch_and_lock(&client, &server, "payment_charge").await;
        assert_eq!(tasks.len(), 1);
        let resp = client
            .post(format!(
                "{}/api/v1/process-instances/{}/cancel",
                server.base_url, instance2
            ))
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();
        let resp = client
            .post(format!(
                "{}/api/v1/external-tasks/{}/complete",
                server.base_url, task_id
            ))
            .header("Authorization", bearer("worker", TENANT, "worker-1"))
            .json(&serde_json::json!({ "worker_id": "worker-1", "variables": {} }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 409);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"], "instance_cancelled");

        server.stop().await;
    }
}
