//! REST-level integration tests for the structured variable & artifact URI
//! contract (E2-3, issue #13), memory storage mode.
//!
//! Reuses the E1-1 harness pattern from `storage_memory_mode.rs`: build the
//! full app via `build_app`, serve on an ephemeral port, exercise the REST
//! API over real HTTP. Covers:
//!
//! - json-envelope variables stored verbatim + plain legacy variables still
//!   driving an exclusive gateway,
//! - artifact envelope URI validation (accepted / each rejection reason),
//! - the `variable_max_bytes` cap at all three write boundaries
//!   (start-instance, user-task complete, external-task complete),
//! - malformed / future-version envelope rejection,
//! - `sensitive` ⇒ must-be-encrypted with ciphertext-only history,
//! - `history_variable_max_bytes` redaction (`__truncated:` marker) while
//!   the instance keeps the full value,
//! - fetch-and-lock delivering envelope strings to workers verbatim.

use bpm_engine_server_rest::bpm_config::{BpmConfig, DatabaseConfig, ServerConfig};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// BPMN with an exclusive gateway so a plain legacy variable decides routing.
const GATEWAY_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="gw-probe" isExecutable="true">
    <startEvent id="start"/>
    <exclusiveGateway id="decide" default="f-else"/>
    <endEvent id="end-yes"/>
    <endEvent id="end-no"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="decide"/>
    <sequenceFlow id="f2" sourceRef="decide" targetRef="end-yes">
      <conditionExpression>${approved == true}</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f-else" sourceRef="decide" targetRef="end-no"/>
  </process>
</definitions>"#;

/// BPMN with a UserTask so the started instance blocks (stays Running).
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

/// 32-byte AES-256 key as hex (test-only).
const TEST_KEY_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

const JSON_ENVELOPE: &str = r#"__var:1:{"t":"json","val":{"items":[1,2,3],"ok":true}}"#;
const ARTIFACT_ENVELOPE: &str = r#"__var:1:{"t":"artifact","uri":"https://storage.example.com/reports/2026-q3.pdf","size":102400,"sha256":"a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90","media_type":"application/pdf"}"#;
const RESERVED_ARTIFACT_ENVELOPE: &str =
    r#"__var:1:{"t":"artifact","uri":"artifact://reports/2026/q3"}"#;

struct TestServer {
    base_url: String,
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
) -> Result<serde_json::Value, (reqwest::StatusCode, serde_json::Value)> {
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
        Ok(body)
    } else {
        Err((status, body))
    }
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

/// Find the payload of the first event with the given type.
fn history_payload(history: &serde_json::Value, event_type: &str) -> serde_json::Value {
    history["events"]
        .as_array()
        .expect("events array")
        .iter()
        .find(|e| e["event_type"] == event_type)
        .map(|e| e["payload"].clone())
        .unwrap_or_else(|| panic!("no {event_type} event in history: {history}"))
}

fn var_map(entries: &[(&str, &str)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in entries {
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    serde_json::Value::Object(map)
}

// ---------------------------------------------------------------------------
// Envelope storage + gateway on plain variables
// ---------------------------------------------------------------------------

#[tokio::test]
async fn json_envelope_stored_verbatim_and_plain_variable_drives_gateway() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, GATEWAY_BPMN).await;

    // A json-envelope variable AND a plain legacy variable; the plain one
    // drives the exclusive gateway condition.
    let body = start_instance(
        &client,
        &server,
        "gw-probe",
        var_map(&[("payload", JSON_ENVELOPE), ("approved", "true")]),
    )
    .await
    .expect("start should succeed");
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    // GET shows the envelope string verbatim (readers see envelope strings
    // as-is; only __encrypted: values are decrypted).
    let inst = get_instance(&client, &server, &instance_id).await;
    assert_eq!(inst["variables"]["payload"], JSON_ENVELOPE);
    assert_eq!(inst["status"], "COMPLETED");

    // The plain variable routed the token to end-yes, not end-no.
    let history = get_history(&client, &server, &instance_id).await;
    let arrived_nodes: Vec<&str> = history["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event_type"] == "TokenArrived")
        .map(|e| e["payload"]["node_id"].as_str().unwrap())
        .collect();
    assert!(
        arrived_nodes.contains(&"end-yes"),
        "token should route to end-yes via the plain variable, got {arrived_nodes:?}"
    );
    assert!(
        !arrived_nodes.contains(&"end-no"),
        "token must not route to end-no, got {arrived_nodes:?}"
    );

    // ProcessStarted history keeps the small envelope verbatim.
    let started = history_payload(&history, "ProcessStarted");
    assert_eq!(started["initial_variables"]["payload"], JSON_ENVELOPE);

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Artifact envelopes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn artifact_envelope_valid_accepted_and_invalid_uri_rejected() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, GATEWAY_BPMN).await;

    // Valid https artifact envelope (with metadata) and the reserved
    // artifact:// scheme (valid but opaque) are both accepted.
    let body = start_instance(
        &client,
        &server,
        "gw-probe",
        var_map(&[
            ("report", ARTIFACT_ENVELOPE),
            ("reserved", RESERVED_ARTIFACT_ENVELOPE),
            ("approved", "false"),
        ]),
    )
    .await
    .expect("valid artifact envelopes must be accepted");
    let instance_id = body["instance_id"].as_str().unwrap().to_string();
    let inst = get_instance(&client, &server, &instance_id).await;
    assert_eq!(inst["variables"]["report"], ARTIFACT_ENVELOPE);
    assert_eq!(inst["variables"]["reserved"], RESERVED_ARTIFACT_ENVELOPE);

    // Each invalid URI shape is rejected with the specific error code.
    let long_uri = format!("https://storage.example.com/{}", "a".repeat(2100));
    let bad: Vec<(&str, &str, &str)> = vec![
        (
            "userinfo",
            "https://user:secret@storage.example.com/report.pdf",
            "userinfo",
        ),
        (
            "fragment",
            "https://storage.example.com/report.pdf#page=3",
            "fragment",
        ),
        ("too-long", &long_uri, "2048"),
        (
            "bad-sha256",
            r#"__var:1:{"t":"artifact","uri":"https://storage.example.com/a","sha256":"abcd"}"#,
            "64 hex",
        ),
    ];
    for (label, value, expected_detail) in bad {
        // userinfo / fragment / too-long cases submit a raw URI string as
        // the envelope value (built below); bad-sha256 is already an
        // envelope. Normalize: wrap raw URIs into an artifact envelope.
        let envelope = if value.starts_with("__var:1:") {
            value.to_string()
        } else {
            format!(
                r#"__var:1:{{"t":"artifact","uri":{}}}"#,
                serde_json::to_string(value).unwrap()
            )
        };
        let (status, error) = start_instance(
            &client,
            &server,
            "gw-probe",
            var_map(&[("artifact", &envelope)]),
        )
        .await
        .expect_err("invalid artifact URI must be rejected");
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "case {label}");
        assert_eq!(
            error["code"], "variable_invalid_artifact_uri",
            "case {label}: wrong error body {error}"
        );
        assert!(
            error["error"].as_str().unwrap().contains("artifact"),
            "case {label}: error should name the variable"
        );
        assert!(
            error["error"].as_str().unwrap().contains(expected_detail),
            "case {label}: reason should mention {expected_detail}: {error}"
        );
    }

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Size cap + malformed envelope at start-instance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oversized_variable_rejected_with_code_name_and_hint() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, GATEWAY_BPMN).await;

    let oversized = "x".repeat(70 * 1024); // 70 KiB > 65_536 default cap
    let (status, error) = start_instance(
        &client,
        &server,
        "gw-probe",
        var_map(&[("big", &oversized)]),
    )
    .await
    .expect_err("oversized variable must be rejected");
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error["code"], "variable_too_large");
    let message = error["error"].as_str().unwrap();
    assert!(message.contains("big"), "must name the variable: {message}");
    assert!(message.contains("65536"), "must state the limit: {message}");
    assert!(
        message.contains("store large payloads externally"),
        "must include the artifact-envelope hint: {message}"
    );
    assert!(
        message.contains("71680"),
        "must state the actual size (70 KiB = 71680 bytes): {message}"
    );

    server.stop().await;
}

#[tokio::test]
async fn malformed_and_future_version_envelopes_rejected() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, GATEWAY_BPMN).await;

    for (label, value, expected_reason) in [
        ("malformed-json", "__var:1:{oops", "invalid JSON"),
        (
            "unknown-version",
            r#"__var:2:{"t":"json","val":1}"#,
            "unsupported envelope version",
        ),
    ] {
        let (status, error) =
            start_instance(&client, &server, "gw-probe", var_map(&[("bad", value)]))
                .await
                .expect_err("invalid envelope must be rejected");
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "case {label}");
        assert_eq!(
            error["code"], "variable_invalid_envelope",
            "case {label}: wrong error body {error}"
        );
        let message = error["error"].as_str().unwrap();
        assert!(message.contains("bad"), "must name the variable: {message}");
        assert!(
            message.contains(expected_reason),
            "case {label}: message should carry the reason '{expected_reason}': {message}"
        );
    }

    server.stop().await;
}

// ---------------------------------------------------------------------------
// sensitive ⇒ must be encrypted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sensitive_requires_encryption_and_history_stays_ciphertext() {
    // This test is the ONLY one in this binary that manipulates the
    // process-global BPM_VARIABLE_ENCRYPTION_KEY (including the inflated
    // ciphertext case below): no concurrent test can race the env var.
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, USER_TASK_BPMN).await;
    deploy(&client, &server, GATEWAY_BPMN).await;
    std::env::set_var("BPM_VARIABLE_ENCRYPTION_KEY", TEST_KEY_HEX);

    // sensitive without encrypted → 4xx
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "user-task-probe",
            "variables": { "secret": "swordfish" },
            "sensitive": ["secret"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "sensitive_requires_encryption");
    assert!(body["error"].as_str().unwrap().contains("secret"));

    // sensitive name not present in the submitted variables → 4xx
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "user-task-probe",
            "variables": { "other": "value" },
            "sensitive": ["missing"],
            "encrypted": ["secret"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "sensitive_requires_encryption");
    assert!(body["error"].as_str().unwrap().contains("missing"));

    // sensitive + encrypted + key configured → instance created.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "user-task-probe",
            "variables": { "secret": "swordfish", "plain": "ok" },
            "encrypted": ["secret"],
            "sensitive": ["secret"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "sensitive+encrypted must be accepted");
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    // GET decrypts for the API caller holding the key (display masking is
    // a UI concern, E6).
    let inst = get_instance(&client, &server, &instance_id).await;
    assert_eq!(inst["variables"]["secret"], "swordfish");
    assert_eq!(inst["variables"]["plain"], "ok");

    // History for that event contains __encrypted: ciphertext, not the
    // plaintext.
    let history = get_history(&client, &server, &instance_id).await;
    let started = history_payload(&history, "ProcessStarted");
    let stored_secret = started["initial_variables"]["secret"].as_str().unwrap();
    assert!(
        stored_secret.starts_with("__encrypted:"),
        "history must hold ciphertext, got {stored_secret}"
    );
    assert!(
        !serde_json::to_string(&started)
            .unwrap()
            .contains("swordfish"),
        "history payload must not contain the plaintext anywhere"
    );

    // The cap applies to the FINAL stored string: plaintext just under the
    // cap is rejected when the inflated ciphertext exceeds it.
    let almost = "s".repeat(65_500);
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "gw-probe",
            "variables": { "secret": almost },
            "encrypted": ["secret"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "inflated ciphertext must be rejected");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "variable_too_large");
    assert!(body["error"].as_str().unwrap().contains("secret"));

    std::env::remove_var("BPM_VARIABLE_ENCRYPTION_KEY");
    server.stop().await;
}

// ---------------------------------------------------------------------------
// History redaction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn large_variable_redacted_in_history_but_kept_on_instance() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, GATEWAY_BPMN).await;

    let two_kib = "x".repeat(2048);
    let at_limit = "y".repeat(1024);
    let body = start_instance(
        &client,
        &server,
        "gw-probe",
        var_map(&[("big", &two_kib), ("at_limit", &at_limit)]),
    )
    .await
    .expect("start should succeed");
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    // Instance GET returns the full value — the cap only applies to history.
    let inst = get_instance(&client, &server, &instance_id).await;
    assert_eq!(inst["variables"]["big"].as_str().unwrap().len(), 2048);

    // The ProcessStarted history event shows the __truncated: marker with
    // the correct original length (2048 = 0x800).
    let history = get_history(&client, &server, &instance_id).await;
    let started = history_payload(&history, "ProcessStarted");
    let redacted = started["initial_variables"]["big"].as_str().unwrap();
    assert!(
        redacted.starts_with("__truncated:2048:"),
        "expected __truncated:2048: marker, got {redacted}"
    );
    // Marker shape: __truncated:<len>:<12 hex>:
    let marker_len = "__truncated:2048:".len() + 12 + 1;
    assert_eq!(redacted.len(), marker_len, "marker shape: {redacted}");

    // A value exactly at the 1024-byte limit is kept verbatim.
    assert_eq!(
        started["initial_variables"]["at_limit"]
            .as_str()
            .unwrap()
            .len(),
        1024
    );
    assert!(started["initial_variables"]["at_limit"]
        .as_str()
        .unwrap()
        .starts_with('y'));

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Fetch-and-lock verbatim delivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_and_lock_returns_envelope_strings_verbatim() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    // payment-flow is a bootstrap definition (memory mode) with an
    // external task of type "payment".
    let body = start_instance(
        &client,
        &server,
        "payment-flow",
        var_map(&[("payload", JSON_ENVELOPE), ("report", ARTIFACT_ENVELOPE)]),
    )
    .await
    .expect("start should succeed");
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    // Fetch-and-lock: the worker receives the envelope strings verbatim and
    // parses them per docs/variables-and-artifacts.md.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .json(&serde_json::json!({
            "worker_id": "w-contract",
            "task_types": ["payment"],
            "max_tasks": 1,
            "lock_duration_ms": 60_000,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tasks: serde_json::Value = resp.json().await.unwrap();
    let tasks = tasks.as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1, "worker should lock the payment task");
    assert_eq!(tasks[0]["process_instance_id"], instance_id);
    let variables = tasks[0]["variables"].as_object().expect("variables map");
    assert_eq!(
        variables.get("payload").and_then(|v| v.as_str()),
        Some(JSON_ENVELOPE),
        "json envelope must be delivered verbatim"
    );
    assert_eq!(
        variables.get("report").and_then(|v| v.as_str()),
        Some(ARTIFACT_ENVELOPE),
        "artifact envelope must be delivered verbatim"
    );

    // Completing with a valid small envelope variable is accepted.
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-contract",
            "variables": { "result": JSON_ENVELOPE },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "envelope at ext-complete: {resp:?}");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Caps at the other write boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn caps_enforced_at_user_task_complete() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server, USER_TASK_BPMN).await;

    let body = start_instance(&client, &server, "user-task-probe", var_map(&[]))
        .await
        .expect("start should succeed");
    let instance_id = body["instance_id"].as_str().unwrap().to_string();
    let task_id = format!("{}:review", instance_id);

    // Oversized variable → 4xx variable_too_large naming the variable.
    let oversized = "z".repeat(70 * 1024);
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({ "variables": { "big": oversized } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "variable_too_large");
    assert!(error["error"].as_str().unwrap().contains("big"));

    // Malformed envelope → 4xx variable_invalid_envelope.
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({ "variables": { "bad": "__var:1:{oops" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "variable_invalid_envelope");

    // The task is still waiting: rejections happened before the engine ran.
    let inst = get_instance(&client, &server, &instance_id).await;
    let current_nodes: Vec<String> = inst["current_nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        current_nodes.contains(&"review".to_string()),
        "instance must still wait at 'review' after rejections"
    );

    // A valid completion still works afterwards.
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "variables": { "result": JSON_ENVELOPE, "note": "fine" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    server.stop().await;
}

#[tokio::test]
async fn caps_enforced_at_external_task_complete() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let body = start_instance(&client, &server, "payment-flow", var_map(&[]))
        .await
        .expect("start should succeed");
    let _instance_id = body["instance_id"].as_str().unwrap().to_string();

    // Lock the task.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .json(&serde_json::json!({
            "worker_id": "w-cap",
            "task_types": ["payment"],
            "max_tasks": 1,
            "lock_duration_ms": 60_000,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tasks: serde_json::Value = resp.json().await.unwrap();
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();

    // Oversized variable → 4xx variable_too_large; the task is untouched
    // (still completable with valid variables by the same owner).
    let oversized = "z".repeat(70 * 1024);
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-cap",
            "variables": { "big": oversized },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "variable_too_large");
    assert!(error["error"].as_str().unwrap().contains("big"));

    // Completing with an invalid artifact URI → 4xx with that code.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-cap",
            "variables": { "a": r#"__var:1:{"t":"artifact","uri":"https://u@x.example.com/a"}"# },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "variable_invalid_artifact_uri");

    // Valid variables complete fine.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .json(&serde_json::json!({
            "worker_id": "w-cap",
            "variables": { "note": "ok" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    server.stop().await;
}
