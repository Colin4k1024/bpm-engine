//! REST-level tests for audit & data retention (E5-3, issue #25).
//!
//! The tests run with JWT auth configured (like the approval/rbac suites:
//! the secret is fixed for the whole test PROCESS) and with
//! `engine.approval_required_task_types = ["payment_charge"]` so the
//! approval flow exercises the full actor chain (start → decide →
//! fetch/complete):
//!
//! - **Audit endpoint** (`GET /api/v1/process-instances/:id/audit`): the
//!   actor-attributed timeline (start by alice, approval granted by bob,
//!   task completed by worker-x — engine-internal events listed WITHOUT
//!   an actor), the definition id+version+status, approval decisions
//!   (actor + note), artifact references parsed from `__var:1:`
//!   envelopes; unknown instances and cross-tenant access answer 404
//!   `not_found`.
//! - **Redaction**: variable values in the audit payload carry the E2-3
//!   markers (`__truncated:`), never the oversized plaintext; encrypted
//!   values stay CIPHERTEXT (the audit endpoint never decrypts).
//! - **Replay secrecy** (the key acceptance, 回放不泄露密钥): replay
//!   snapshot/step/seek responses expose no variable material at all —
//!   the raw bodies contain neither plaintext secrets nor ciphertext.
//! - **Retention**: with `audit.retention_days = 1` and injected OLD
//!   history/dead-letter rows, the sweep deletes ONLY the aged rows
//!   (recent kept, idempotent on re-run) and `/ready` reports the
//!   retention check; with the default `retention_days = 0` nothing is
//!   ever deleted and `/ready` carries no retention key.
//!
//! Memory driver — runs in CI by default (no Docker). The store-level
//! `delete_older_than` round-trips (history + dead letters, numeric TEXT
//! comparison) are covered by
//! `crates/adapters/postgres/tests/integration.rs` (Docker-gated) and the
//! memory adapters' own unit tests.

use bpm_engine_server_rest::audit::run_retention_sweep;
use bpm_engine_server_rest::bpm_config::{
    AuditConfig, AuthConfig, BpmConfig, DatabaseConfig, EngineConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::state::AppState;
use bpm_engine_server_rest::{build_app, middleware::mint_jwt};
use bpm_engine_storage::{DeadLetterEntry, HistoryRepo};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const TEST_SECRET: &str = "audit-test-secret";
const TENANT: &str = "acme";
/// 32-byte AES key (hex) for the encrypted-variable flows. Process-global
/// env var — set with the SAME value by every test that needs it (the
/// pattern of variable_contract.rs / step_trace.rs).
const TEST_KEY_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// A plaintext secret that must NEVER appear in an audit or replay body.
const PLAINTEXT_SECRET: &str = "swordfish-e53";
/// The SHA-256 of the test artifact (exactly 64 hex chars, shape only).
fn artifact_sha() -> String {
    "a".repeat(64)
}

/// An approval-required external task flow (the same shape as the
/// approval-semantics suite).
const APPROVAL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="audit-approval-flow" isExecutable="true">
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

/// A user-task flow (the operator-completed task boundary event).
const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="audit-review-flow" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;

/// The artifact envelope of the test flow's `report` variable.
fn artifact_envelope_value() -> String {
    format!(
        r#"__var:1:{{"t":"artifact","uri":"https://storage.example.com/reports/audit-r1.md","sha256":"{}","size":4096,"media_type":"text/markdown"}}"#,
        artifact_sha()
    )
}

struct TestServer {
    base_url: String,
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
            timer_poll_interval_ms: 200,
            timer_batch_size: 100,
            ..Default::default()
        },
        rate_limit: RateLimitConfig {
            requests_per_minute: 1_000_000,
        },
        // Retention OFF by default (the safe default; the retention tests
        // build their own config).
        audit: AuditConfig::default(),
        ..Default::default()
    }
}

async fn spawn_server_with(cfg: BpmConfig) -> TestServer {
    let app = build_app(&cfg).await.expect("build_app failed");
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

async fn spawn_server() -> TestServer {
    spawn_server_with(config()).await
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

/// A bearer token for `(role, tenant, sub)`.
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
        .header("Authorization", bearer("deployer", TENANT, "dev"))
        .body(bpmn.to_string())
        .send()
        .await
        .expect("deploy request");
    assert_eq!(resp.status(), 201, "deploy should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["process_definition_id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Flow helpers
// ---------------------------------------------------------------------------

/// Start an instance as the operator `sub`; `extra` is merged into the
/// request body (variables/encrypted/sensitive).
async fn start_instance_extra(
    client: &reqwest::Client,
    server: &TestServer,
    sub: &str,
    def: &str,
    extra: serde_json::Value,
) -> String {
    let mut body = serde_json::json!({ "process_def_id": def });
    if let (Some(obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("operator", TENANT, sub))
        .json(&body)
        .send()
        .await
        .expect("start request");
    let status = resp.status();
    let text = resp.text().await.expect("start body");
    assert_eq!(status, 201, "start should return 201: {text}");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("start body is JSON");
    parsed["instance_id"].as_str().unwrap().to_string()
}

/// GET the instance (operator read).
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

/// Poll GET instance until `status` equals `expected`.
async fn wait_for_status(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
    expected: &str,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let inst = get_instance(client, server, id).await;
        if inst["status"].as_str() == Some(expected) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "instance did not reach {expected} within {timeout:?}: {inst}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The instance's approval records (operator read).
async fn list_approvals(
    client: &reqwest::Client,
    server: &TestServer,
    id: &str,
) -> Vec<serde_json::Value> {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/approvals",
            server.base_url, id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("list approvals");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    body["approvals"].as_array().unwrap().clone()
}

/// Decide an approval as the operator `sub`.
async fn decide_as(
    client: &reqwest::Client,
    server: &TestServer,
    approval_id: &str,
    sub: &str,
    decision: &str,
    note: Option<&str>,
) {
    let resp = client
        .post(format!(
            "{}/api/v1/approvals/{}/decide",
            server.base_url, approval_id
        ))
        .header("Authorization", bearer("operator", TENANT, sub))
        .json(&serde_json::json!({ "decision": decision, "note": note }))
        .send()
        .await
        .expect("decide request");
    assert_eq!(resp.status(), 200, "decide should return 200");
}

/// Fetch-and-lock as the worker `sub`; returns the delivered tasks.
async fn fetch_and_lock_as(
    client: &reqwest::Client,
    server: &TestServer,
    sub: &str,
    task_type: &str,
) -> Vec<serde_json::Value> {
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT, sub))
        .json(&serde_json::json!({
            "worker_id": sub,
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

/// GET the audit view as `role`/`sub`; returns (status, raw text).
async fn get_audit_raw(
    client: &reqwest::Client,
    server: &TestServer,
    role: &str,
    sub: &str,
    tenant: &str,
    id: &str,
) -> (reqwest::StatusCode, String) {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/audit",
            server.base_url, id
        ))
        .header("Authorization", bearer(role, tenant, sub))
        .send()
        .await
        .expect("audit request");
    let status = resp.status();
    (status, resp.text().await.expect("audit body"))
}

// ---------------------------------------------------------------------------
// Audit endpoint: actor, definition version, decisions, artifacts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_endpoint_joins_actor_decisions_definition_and_artifacts() {
    std::env::set_var("BPM_VARIABLE_ENCRYPTION_KEY", TEST_KEY_HEX);
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server, APPROVAL_BPMN).await;

    // alice starts the instance: an artifact envelope, an
    // encrypted+sensitive secret, and a public variable.
    let instance_id = start_instance_extra(
        &client,
        &server,
        "alice",
        "audit-approval-flow",
        serde_json::json!({
            "variables": {
                "report": artifact_envelope_value(),
                "secret": PLAINTEXT_SECRET,
                "topic": "q3-close",
            },
            "encrypted": ["secret"],
            "sensitive": ["secret"],
        }),
    )
    .await;

    // The approval record exists after the (synchronous) start.
    let approvals = list_approvals(&client, &server, &instance_id).await;
    assert_eq!(
        approvals.len(),
        1,
        "one approval-required step: {approvals:?}"
    );
    let approval_id = approvals[0]["approval_id"].as_str().unwrap().to_string();

    // bob grants it (with a note).
    decide_as(
        &client,
        &server,
        &approval_id,
        "bob",
        "approve",
        Some("risk checked"),
    )
    .await;

    // worker-x fetches and completes the now-created task.
    let tasks = fetch_and_lock_as(&client, &server, "worker-x", "payment_charge").await;
    assert_eq!(tasks.len(), 1, "the approved task is fetchable: {tasks:?}");
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-x"))
        .json(&serde_json::json!({
            "worker_id": "worker-x",
            "variables": { "result": "charged" },
        }))
        .send()
        .await
        .expect("complete request");
    assert_eq!(resp.status(), 200, "worker-x completes the task");
    wait_for_status(
        &client,
        &server,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(10),
    )
    .await;

    // carol (auditor) reads the audit view.
    let (status, raw) =
        get_audit_raw(&client, &server, "auditor", "carol", TENANT, &instance_id).await;
    assert_eq!(status, 200, "audit view: {raw}");
    let body: serde_json::Value = serde_json::from_str(&raw).expect("audit body is JSON");

    // Definition context: the versioned id + record metadata.
    assert_eq!(body["definition"]["id"], "audit-approval-flow");
    assert_eq!(body["definition"]["key"], "audit-approval-flow");
    assert_eq!(body["definition"]["version"], 1);
    assert_eq!(body["definition"]["status"], "active");
    // Instance context: terminal state + tenant.
    assert_eq!(body["instance"]["state"], "COMPLETED");
    assert_eq!(body["instance"]["tenant_id"], TENANT);
    assert_eq!(body["instance_id"], instance_id);

    // Actor-carrying boundary events, by type.
    let events = body["events"].as_array().expect("events array");
    let find = |event_type: &str| {
        events
            .iter()
            .find(|e| e["event_type"].as_str() == Some(event_type))
            .unwrap_or_else(|| panic!("audit must list {event_type}: {raw}"))
            .clone()
    };
    let started = find("InstanceStarted");
    assert_eq!(
        started["actor"], "alice",
        "the START boundary carries alice"
    );
    assert_eq!(started["payload"]["process_def_id"], "audit-approval-flow");

    let granted = find("ApprovalGranted");
    assert_eq!(granted["actor"], "bob", "the DECISION carries bob");
    assert_eq!(granted["payload"]["note"], "risk checked");

    let locked = find("ExternalTaskLocked");
    assert_eq!(
        locked["actor"], "worker-x",
        "the lease boundary carries the verified principal"
    );
    assert_eq!(locked["payload"]["worker_id"], "worker-x");

    let completed = find("TaskCompleted");
    assert_eq!(completed["actor"], "worker-x");
    assert_eq!(completed["payload"]["task_kind"], "external");
    assert_eq!(completed["payload"]["worker_id"], "worker-x");

    // Engine-internal transitions are listed WITHOUT an actor (by design).
    for internal in ["ProcessStarted", "TokenArrived", "ProcessCompleted"] {
        let ev = find(internal);
        assert!(
            ev.get("actor").is_none(),
            "engine-internal {internal} must not fake an actor: {ev}"
        );
    }

    // Decisions: the single approval decision with actor and note.
    let decisions = body["decisions"].as_array().expect("decisions array");
    assert_eq!(decisions.len(), 1, "one decision: {decisions:?}");
    assert_eq!(decisions[0]["approval_id"], approval_id);
    assert_eq!(decisions[0]["decision"], "approved");
    assert_eq!(decisions[0]["actor"], "bob");
    assert_eq!(decisions[0]["note"], "risk checked");
    assert_eq!(decisions[0]["node_id"], "charge");

    // Artifacts: the envelope reference with sha256/size.
    let artifacts = body["artifacts"].as_array().expect("artifacts array");
    assert_eq!(artifacts.len(), 1, "one artifact reference: {artifacts:?}");
    assert_eq!(artifacts[0]["variable"], "report");
    assert_eq!(
        artifacts[0]["uri"],
        "https://storage.example.com/reports/audit-r1.md"
    );
    assert_eq!(artifacts[0]["sha256"], artifact_sha());
    assert_eq!(artifacts[0]["size"], 4096);
    assert_eq!(artifacts[0]["media_type"], "text/markdown");
    assert_eq!(artifacts[0]["source"], "history");

    // Secrecy: the audit payload NEVER decrypts — the secret stays
    // ciphertext; the plaintext is nowhere in the body.
    assert!(
        !raw.contains(PLAINTEXT_SECRET),
        "audit must never expose the plaintext secret: {raw}"
    );
    assert!(
        raw.contains("__encrypted:"),
        "the encrypted variable appears as ciphertext (never decrypted)"
    );

    // Unknown instance → 404 not_found.
    let (status, raw) = get_audit_raw(
        &client,
        &server,
        "auditor",
        "carol",
        TENANT,
        "no-such-instance",
    )
    .await;
    assert_eq!(status, 404, "unknown instance: {raw}");
    assert!(raw.contains("not_found"), "the 404 carries the stable code");

    // Cross-tenant (E5-1): another tenant's principal sees the SAME 404.
    let (status, raw) = get_audit_raw(
        &client,
        &server,
        "auditor",
        "mallory",
        "other",
        &instance_id,
    )
    .await;
    assert_eq!(status, 404, "cross-tenant audit is gated: {raw}");
    assert!(raw.contains("not_found"));

    // RBAC: the audit read is available to every authenticated role.
    for (role, sub) in [
        ("operator", "alice"),
        ("worker", "worker-x"),
        ("deployer", "dev"),
    ] {
        let (status, raw) = get_audit_raw(&client, &server, role, sub, TENANT, &instance_id).await;
        assert_eq!(status, 200, "{role} may read the audit view: {raw}");
    }

    server.stop().await;
}

/// The user-task boundary: the operator's completion carries the actor.
#[tokio::test]
async fn user_task_completion_boundary_event_carries_actor() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server, USER_TASK_BPMN).await;
    let instance_id = start_instance_extra(
        &client,
        &server,
        "alice",
        "audit-review-flow",
        serde_json::json!({}),
    )
    .await;

    // Complete the user task as alice (operator).
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{}:review/complete",
            server.base_url, instance_id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .json(&serde_json::json!({ "variables": { "decision": "ok" } }))
        .send()
        .await
        .expect("complete request");
    assert_eq!(
        resp.status(),
        200,
        "user task completion: {:?}",
        resp.text().await
    );
    wait_for_status(
        &client,
        &server,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(10),
    )
    .await;

    let (status, raw) =
        get_audit_raw(&client, &server, "auditor", "carol", TENANT, &instance_id).await;
    assert_eq!(status, 200, "audit view: {raw}");
    let body: serde_json::Value = serde_json::from_str(&raw).expect("audit body is JSON");
    let events = body["events"].as_array().expect("events array");
    let completed = events
        .iter()
        .find(|e| e["event_type"].as_str() == Some("TaskCompleted"))
        .expect("the user-task boundary event is listed");
    assert_eq!(completed["actor"], "alice");
    assert_eq!(completed["payload"]["task_kind"], "user");
    assert_eq!(completed["payload"]["node_id"], "review");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Redaction: markers, not values
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_shows_redaction_markers_not_oversized_values() {
    // A tiny history redaction cap: any longer variable value becomes a
    // __truncated: marker in history — and therefore in the audit view.
    let mut cfg = config();
    cfg.engine.history_variable_max_bytes = 8;
    let server = spawn_server_with(cfg).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, USER_TASK_BPMN).await;
    let oversized = "x".repeat(50);
    let instance_id = start_instance_extra(
        &client,
        &server,
        "alice",
        "audit-review-flow",
        serde_json::json!({ "variables": { "big": oversized.clone() } }),
    )
    .await;

    let (status, raw) =
        get_audit_raw(&client, &server, "auditor", "carol", TENANT, &instance_id).await;
    assert_eq!(status, 200, "audit view: {raw}");
    let body: serde_json::Value = serde_json::from_str(&raw).expect("audit body is JSON");
    let events = body["events"].as_array().expect("events array");
    let started = events
        .iter()
        .find(|e| e["event_type"].as_str() == Some("ProcessStarted"))
        .expect("ProcessStarted is listed");
    let big = started["payload"]["initial_variables"]["big"]
        .as_str()
        .expect("the redacted marker is a string");
    assert!(
        big.starts_with("__truncated:50:"),
        "oversized values appear as markers, got {big}"
    );
    assert!(!raw.contains(&oversized), "the value never surfaces: {raw}");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Replay secrecy: 回放不泄露密钥
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_snapshot_and_steps_never_expose_secrets() {
    std::env::set_var("BPM_VARIABLE_ENCRYPTION_KEY", TEST_KEY_HEX);
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    // The memory bootstrap definition `minimal` runs to completion at
    // once — the history carries the (ciphertext) initial variables.
    let instance_id = start_instance_extra(
        &client,
        &server,
        "alice",
        "minimal",
        serde_json::json!({
            "variables": { "secret": PLAINTEXT_SECRET, "topic": "public-topic" },
            "encrypted": ["secret"],
            "sensitive": ["secret"],
        }),
    )
    .await;
    wait_for_status(
        &client,
        &server,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(10),
    )
    .await;

    // Create a replay session (operator).
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/replay",
            server.base_url, instance_id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("replay create");
    assert_eq!(resp.status(), 201, "replay session created");
    let create_raw = resp.text().await.expect("replay create body");
    let create: serde_json::Value = serde_json::from_str(&create_raw).expect("replay body JSON");
    let session_id = create["session_id"].as_str().unwrap().to_string();
    let total_events = create["total_events"].as_u64().unwrap();
    assert!(total_events > 0, "the instance has replayable events");

    // Secrecy on the CREATE response.
    assert!(
        !create_raw.contains(PLAINTEXT_SECRET) && !create_raw.contains("__encrypted:"),
        "replay create leaks no variable material: {create_raw}"
    );

    // Step through EVERY event, keeping the RAW bodies.
    let mut bodies: Vec<String> = Vec::new();
    for _ in 0..=total_events {
        let resp = client
            .post(format!(
                "{}/api/v1/replay/{}/step",
                server.base_url, session_id
            ))
            .header("Authorization", bearer("operator", TENANT, "alice"))
            .send()
            .await
            .expect("replay step");
        if resp.status() != 200 {
            break; // "no more events to step" (400)
        }
        bodies.push(resp.text().await.expect("replay step body"));
    }
    assert!(!bodies.is_empty(), "at least one step must succeed");
    for body in &bodies {
        assert!(
            !body.contains(PLAINTEXT_SECRET),
            "replay step must never expose the plaintext secret: {body}"
        );
        assert!(
            !body.contains("__encrypted:"),
            "replay step exposes no variable material at all (not even ciphertext): {body}"
        );
        assert!(
            !body.contains("public-topic"),
            "plain variables are not surfaced either: {body}"
        );
    }

    // Snapshot (raw body) after the steps.
    let resp = client
        .get(format!(
            "{}/api/v1/replay/{}/snapshot",
            server.base_url, session_id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("replay snapshot");
    assert_eq!(resp.status(), 200, "snapshot readable");
    let snapshot_raw = resp.text().await.expect("snapshot body");
    assert!(
        !snapshot_raw.contains(PLAINTEXT_SECRET) && !snapshot_raw.contains("__encrypted:"),
        "replay snapshot leaks no variable material: {snapshot_raw}"
    );

    // Seek back to the beginning (re-applies every event) — same check.
    let resp = client
        .post(format!(
            "{}/api/v1/replay/{}/seek",
            server.base_url, session_id
        ))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .json(&serde_json::json!({ "cursor": 0 }))
        .send()
        .await
        .expect("replay seek");
    assert_eq!(resp.status(), 200, "seek works");
    let seek_raw = resp.text().await.expect("seek body");
    assert!(
        !seek_raw.contains(PLAINTEXT_SECRET) && !seek_raw.contains("__encrypted:"),
        "replay seek leaks no variable material: {seek_raw}"
    );

    // DELETE closes the by-construction claim: bodyless 204, nothing to leak.
    let resp = client
        .delete(format!("{}/api/v1/replay/{}", server.base_url, session_id))
        .header("Authorization", bearer("operator", TENANT, "alice"))
        .send()
        .await
        .expect("replay delete");
    assert_eq!(resp.status(), 204, "delete returns no content");
    let delete_raw = resp.text().await.unwrap_or_default();
    assert!(
        delete_raw.is_empty(),
        "the replay delete body is empty (204): {delete_raw}"
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// An aged dead-letter entry for the retention tests.
fn dl_entry(id: &str, created_at: &str) -> DeadLetterEntry {
    DeadLetterEntry {
        id: id.to_string(),
        task_id: format!("task-{id}"),
        token_id: "tok-1".to_string(),
        process_instance_id: "inst-retention".to_string(),
        task_type: "payment_charge".to_string(),
        error_message: "timeout".to_string(),
        variables: "{}".to_string(),
        tenant_id: Some(TENANT.to_string()),
        created_at: created_at.to_string(),
    }
}

/// Start an instance so REAL (recent) history exists, inject AGED rows
/// through the stores, then sweep.
async fn seeded_retention_server(client: &reqwest::Client, server: &TestServer) -> String {
    deploy(client, server, USER_TASK_BPMN).await;
    let instance_id = start_instance_extra(
        client,
        server,
        "alice",
        "audit-review-flow",
        serde_json::json!({}),
    )
    .await;
    // Aged history rows (unix seconds in the far past) for two instances.
    for (id, instance, occurred_at) in [
        ("h-aged-a", "inst-retention", "1000"),
        ("h-aged-b", &instance_id, "1000"),
    ] {
        HistoryRepo::append(
            server.state.storage.history_repo.as_ref(),
            instance,
            "TokenArrived",
            &serde_json::json!({ "marker": id }),
            occurred_at,
        )
        .await
        .unwrap();
    }
    // Aged + recent dead-letter entries.
    server
        .state
        .storage
        .dead_letter_store
        .insert(&dl_entry("dl-aged", "1000"))
        .await
        .unwrap();
    server
        .state
        .storage
        .dead_letter_store
        .insert(&dl_entry(
            "dl-recent",
            &SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_string(),
        ))
        .await
        .unwrap();
    instance_id
}

#[tokio::test]
async fn retention_sweep_deletes_only_aged_rows_and_reports_ready() {
    let mut cfg = config();
    cfg.audit = AuditConfig {
        retention_days: 1,
        // Long interval: the periodic task must not interfere — the test
        // drives `run_retention_sweep` directly.
        sweep_interval_secs: 3600,
    };
    let server = spawn_server_with(cfg).await;
    let client = reqwest::Client::new();
    let instance_id = seeded_retention_server(&client, &server).await;

    // /ready reports the retention check when enabled (E5-3).
    let resp = client
        .get(format!("{}/ready", server.base_url))
        .send()
        .await
        .expect("ready probe");
    assert_eq!(resp.status(), 200);
    let ready: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        ready["checks"]["audit_retention"], "ok",
        "retention is reported on /ready: {ready}"
    );

    // Sweep #1: the aged rows go, the recent ones stay.
    let outcome = run_retention_sweep(&server.state)
        .await
        .expect("sweep runs");
    assert_eq!(outcome.history_deleted, 2, "both aged history rows");
    assert_eq!(outcome.dead_letters_deleted, 1, "the aged dead letter");
    // The outcome is recorded on the state (for /ready and operators).
    let last = server.state.retention.last_sweep.lock().unwrap().clone();
    assert!(
        matches!(&last, Some(o) if o.history_deleted == 2 && o.dead_letters_deleted == 1),
        "the sweep outcome is recorded: {last:?}"
    );

    let events = HistoryRepo::list_by_instance(
        server.state.storage.history_repo.as_ref(),
        &instance_id,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        events.iter().all(|e| e.occurred_at != "1000"),
        "no aged event remains: {events:?}"
    );
    assert!(!events.is_empty(), "the RECENT events are kept");
    let aged = HistoryRepo::list_by_instance(
        server.state.storage.history_repo.as_ref(),
        "inst-retention",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(aged.is_empty(), "the aged instance's events are gone");

    assert!(
        server
            .state
            .storage
            .dead_letter_store
            .get("dl-aged")
            .await
            .unwrap()
            .is_none(),
        "the aged dead letter is deleted"
    );
    assert!(
        server
            .state
            .storage
            .dead_letter_store
            .get("dl-recent")
            .await
            .unwrap()
            .is_some(),
        "the recent dead letter is kept"
    );

    // Sweep #2: idempotent (nothing left to delete).
    let outcome = run_retention_sweep(&server.state)
        .await
        .expect("sweep re-runs");
    assert_eq!(outcome.history_deleted, 0);
    assert_eq!(outcome.dead_letters_deleted, 0);

    server.stop().await;
}

#[tokio::test]
async fn retention_disabled_by_default_deletes_nothing() {
    // retention_days = 0 (the default): nothing is EVER deleted.
    let server = spawn_server_with(config()).await;
    let client = reqwest::Client::new();
    let instance_id = seeded_retention_server(&client, &server).await;

    let outcome = run_retention_sweep(&server.state)
        .await
        .expect("sweep is a no-op");
    assert_eq!(outcome.history_deleted, 0);
    assert_eq!(outcome.dead_letters_deleted, 0);

    let events = HistoryRepo::list_by_instance(
        server.state.storage.history_repo.as_ref(),
        &instance_id,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        events.iter().any(|e| e.occurred_at == "1000"),
        "aged events are kept when retention is off"
    );
    assert!(
        server
            .state
            .storage
            .dead_letter_store
            .get("dl-aged")
            .await
            .unwrap()
            .is_some(),
        "aged dead letters are kept when retention is off"
    );

    // /ready carries NO retention key when disabled.
    let resp = client
        .get(format!("{}/ready", server.base_url))
        .send()
        .await
        .expect("ready probe");
    let ready: serde_json::Value = resp.json().await.unwrap();
    assert!(
        ready["checks"].get("audit_retention").is_none(),
        "the default (keep everything) needs no readiness signal: {ready}"
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Boundary-event actor pinning: ExternalTaskFailed + DeadLetter ops
// (review round 2: the matrix must be regression-pinned end to end)
// ---------------------------------------------------------------------------

/// Failing an external task, requeueing and deleting its dead letter each
/// record the verified actor in the audit timeline.
#[tokio::test]
async fn failed_tasks_and_dead_letter_ops_carry_the_actor() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // The bootstrap payment flow: start as alice, fetch as worker-x.
    let instance_id = start_instance_extra(
        &client,
        &server,
        "alice",
        "payment-flow",
        serde_json::json!({ "variables": { "amount": "9" } }),
    )
    .await;
    let tasks = fetch_and_lock_as(&client, &server, "worker-x", "payment").await;
    assert_eq!(tasks.len(), 1, "the payment task is delivered");
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();

    // worker-x FAILS the task (transient) — the boundary event must carry
    // the verified actor alongside the client-claimed worker id.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/fail",
            server.base_url, task_id
        ))
        .header("Authorization", bearer("worker", TENANT, "worker-x"))
        .json(&serde_json::json!({ "worker_id": "worker-x", "error": "boom" }))
        .send()
        .await
        .expect("fail request");
    assert_eq!(resp.status(), 200, "fail should return 200");

    // Exhaust the retries (retries start at 3): the task dead-letters.
    for i in 0..2 {
        let tasks = fetch_and_lock_as(&client, &server, "worker-x", "payment").await;
        assert_eq!(tasks.len(), 1, "redelivery {i}");
        // NOTE: the id must go through as_str() — formatting the raw JSON
        // Value would embed the surrounding quotes into the URL.
        let fetched_id = tasks[0]["task_id"].as_str().unwrap().to_string();
        let resp = client
            .post(format!(
                "{}/api/v1/external-tasks/{}/fail",
                server.base_url, fetched_id
            ))
            .header("Authorization", bearer("worker", TENANT, "worker-x"))
            .json(&serde_json::json!({
                "worker_id": "worker-x",
                "error": format!("boom {i}")
            }))
            .send()
            .await
            .expect("fail request");
        assert_eq!(resp.status(), 200, "redelivery fail {i}");
    }

    // The audit timeline: the FIRST ExternalTaskFailed carries worker-x.
    let (status, raw) =
        get_audit_raw(&client, &server, "auditor", "carol", TENANT, &instance_id).await;
    assert_eq!(status, 200, "audit view: {raw}");
    let body: serde_json::Value = serde_json::from_str(&raw).expect("audit body is JSON");
    let failed = body["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["event_type"].as_str() == Some("ExternalTaskFailed"))
        .expect("audit lists ExternalTaskFailed")
        .clone();
    assert_eq!(failed["actor"], "worker-x", "failure boundary: {raw}");
    assert_eq!(
        failed["payload"]["worker_id"], "worker-x",
        "the client-claimed worker id sits next to the actor"
    );

    // bob (operator) requeues the dead letter — DeadLetterRequeued(bob).
    // (Memory-driver honesty: requeue removes the DLQ entry and records the
    // event but does NOT recreate the external task — the E1-1 documented
    // semantics — so the redelivery is not asserted here.)
    let letters = client
        .get(format!("{}/api/v1/dead-letters", server.base_url))
        .header("Authorization", bearer("operator", TENANT, "bob"))
        .send()
        .await
        .expect("dead letters list")
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let entry = letters["entries"]
        .as_array()
        .expect("dead letters array")
        .iter()
        .find(|d| d["process_instance_id"].as_str() == Some(instance_id.as_str()))
        .expect("the failed task dead-lettered")
        .clone();
    let letter_id = entry["id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/dead-letters/{}/requeue",
            server.base_url, letter_id
        ))
        .header("Authorization", bearer("operator", TENANT, "bob"))
        .send()
        .await
        .expect("requeue request");
    assert_eq!(resp.status(), 200, "requeue should return 200");

    // A SECOND instance dead-letters, giving bob a distinct entry to delete
    // — DeadLetterDeleted(bob). (The requeued entry is gone in memory.)
    let instance2 = start_instance_extra(
        &client,
        &server,
        "alice",
        "payment-flow",
        serde_json::json!({ "variables": { "amount": "8" } }),
    )
    .await;
    for _ in 0..3 {
        let tasks = fetch_and_lock_as(&client, &server, "worker-x", "payment").await;
        assert_eq!(tasks.len(), 1, "second instance task delivered");
        let fail_id = tasks[0]["task_id"].as_str().unwrap().to_string();
        let resp = client
            .post(format!(
                "{}/api/v1/external-tasks/{}/fail",
                server.base_url, fail_id
            ))
            .header("Authorization", bearer("worker", TENANT, "worker-x"))
            .json(&serde_json::json!({
                "worker_id": "worker-x",
                "error": "second failure"
            }))
            .send()
            .await
            .expect("fail request");
        assert_eq!(resp.status(), 200, "second-instance fail");
    }
    let letters2 = client
        .get(format!("{}/api/v1/dead-letters", server.base_url))
        .header("Authorization", bearer("operator", TENANT, "bob"))
        .send()
        .await
        .expect("dead letters list 2")
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let entry2 = letters2["entries"]
        .as_array()
        .expect("dead letters array 2")
        .iter()
        .find(|d| d["process_instance_id"].as_str() == Some(instance2.as_str()))
        .expect("the second task dead-lettered")
        .clone();
    let letter2_id = entry2["id"].as_str().unwrap().to_string();
    let resp = client
        .delete(format!(
            "{}/api/v1/dead-letters/{}",
            server.base_url, letter2_id
        ))
        .header("Authorization", bearer("operator", TENANT, "bob"))
        .send()
        .await
        .expect("delete request");
    assert_eq!(resp.status(), 204, "delete returns no content");

    // Final audits: the requeue boundary lives on instance 1's timeline,
    // the delete boundary on instance 2's — both carry bob.
    let (status, raw) =
        get_audit_raw(&client, &server, "auditor", "carol", TENANT, &instance_id).await;
    assert_eq!(status, 200, "final audit view (instance 1): {raw}");
    let body: serde_json::Value = serde_json::from_str(&raw).expect("audit body is JSON");
    let event = body["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["event_type"].as_str() == Some("DeadLetterRequeued"))
        .unwrap_or_else(|| panic!("audit lists DeadLetterRequeued: {raw}"))
        .clone();
    assert_eq!(event["actor"], "bob", "DeadLetterRequeued boundary: {raw}");

    let (status, raw2) =
        get_audit_raw(&client, &server, "auditor", "carol", TENANT, &instance2).await;
    assert_eq!(status, 200, "final audit view (instance 2): {raw2}");
    let body2: serde_json::Value = serde_json::from_str(&raw2).expect("audit body is JSON");
    let event = body2["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["event_type"].as_str() == Some("DeadLetterDeleted"))
        .unwrap_or_else(|| panic!("audit lists DeadLetterDeleted: {raw2}"))
        .clone();
    assert_eq!(event["actor"], "bob", "DeadLetterDeleted boundary: {raw2}");

    server.stop().await;
}
