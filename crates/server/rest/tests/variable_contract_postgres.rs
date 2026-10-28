//! Variable & artifact URI contract (E2-3) on real PostgreSQL.
//!
//! Requires Docker (testcontainers). Run with:
//!
//! ```text
//! cargo test -p bpm-engine-server-rest --test variable_contract_postgres -- --ignored
//! ```
//!
//! Memory-mode coverage lives in `variable_contract.rs`; this variant
//! verifies the same contract round-trips through the PostgreSQL stores:
//! json/artifact envelopes stored and read back verbatim, the
//! `variable_max_bytes` cap enforced at the start-instance boundary, and
//! the `history_variable_max_bytes` redaction (`__truncated:` marker)
//! applied to history writes while the instance keeps the full value.

#![cfg(feature = "postgres")]

use bpm_engine_server_rest::bpm_config::{BpmConfig, DatabaseConfig, ServerConfig};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use testcontainers::clients;
use testcontainers_modules::postgres::Postgres;
use tokio_util::sync::CancellationToken;

/// BPMN with a UserTask so the started instance blocks (stays Running).
const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="vc-pg-probe" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;

const JSON_ENVELOPE: &str = r#"__var:1:{"t":"json","val":{"items":[1,2,3],"ok":true}}"#;
const ARTIFACT_ENVELOPE: &str = r#"__var:1:{"t":"artifact","uri":"https://storage.example.com/reports/2026-q3.pdf","size":102400,"sha256":"a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90","media_type":"application/pdf"}"#;

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
    /// Stop the server: cancel background tasks, abort the serve task
    /// (which drops the router and the app state).
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn postgres_config(url: &str) -> BpmConfig {
    BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "postgres".to_string(),
            url: url.to_string(),
            max_connections: 5,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_variable_contract_roundtrip() {
    let docker: &'static clients::Cli = Box::leak(Box::new(clients::Cli::default()));
    let container = docker.run(Postgres::default());
    let port = container.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);
    let config = postgres_config(&url);

    let client = reqwest::Client::new();
    let server = spawn_server(&config).await;

    // Deploy the blocking user-task definition.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "deploy should return 201");

    // --- Envelope + redaction round-trip -----------------------------------
    let two_kib = "x".repeat(2048);
    let at_limit = "y".repeat(1024);
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "vc-pg-probe",
            "variables": {
                "payload": JSON_ENVELOPE,
                "report": ARTIFACT_ENVELOPE,
                "big": two_kib,
                "at_limit": at_limit,
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "envelopes must be accepted on postgres");
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    // Instance GET returns envelope strings verbatim and the full 2 KiB
    // value — the history cap never truncates instance state.
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let inst: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(inst["variables"]["payload"], JSON_ENVELOPE);
    assert_eq!(inst["variables"]["report"], ARTIFACT_ENVELOPE);
    assert_eq!(inst["variables"]["big"].as_str().unwrap().len(), 2048);
    assert_eq!(inst["variables"]["at_limit"].as_str().unwrap().len(), 1024);
    assert_eq!(inst["status"], "RUNNING");

    // History (persisted via the Postgres HistoryRepo) shows the envelopes
    // verbatim, the 2 KiB value as the __truncated: marker with the correct
    // original length, and the at-limit value verbatim.
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let history: serde_json::Value = resp.json().await.unwrap();
    let started = history["events"]
        .as_array()
        .expect("events array")
        .iter()
        .find(|e| e["event_type"] == "ProcessStarted")
        .expect("ProcessStarted event")["payload"]
        .clone();
    assert_eq!(started["initial_variables"]["payload"], JSON_ENVELOPE);
    assert_eq!(started["initial_variables"]["report"], ARTIFACT_ENVELOPE);
    let redacted = started["initial_variables"]["big"].as_str().unwrap();
    assert!(
        redacted.starts_with("__truncated:2048:"),
        "expected __truncated:2048: marker, got {redacted}"
    );
    let marker_len = "__truncated:2048:".len() + 12 + 1;
    assert_eq!(redacted.len(), marker_len, "marker shape: {redacted}");
    assert_eq!(
        started["initial_variables"]["at_limit"]
            .as_str()
            .unwrap()
            .len(),
        1024
    );

    // --- Cap enforcement at the start boundary ------------------------------
    let oversized = "x".repeat(70 * 1024); // 70 KiB > 65_536 default cap
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "vc-pg-probe",
            "variables": { "big": oversized },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "variable_too_large");
    assert!(error["error"].as_str().unwrap().contains("big"));
    assert!(error["error"].as_str().unwrap().contains("65536"));

    // --- Malformed envelope rejection ---------------------------------------
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "vc-pg-probe",
            "variables": { "bad": "__var:1:{oops" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "variable_invalid_envelope");

    // --- Invalid artifact URI rejection -------------------------------------
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({
            "process_def_id": "vc-pg-probe",
            "variables": {
                "a": "__var:1:{\"t\":\"artifact\",\"uri\":\"https://user:pw@storage.example.com/x\"}"
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let error: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(error["code"], "variable_invalid_artifact_uri");

    // --- Survives restart (state + history redaction are persisted) ---------
    server.stop().await;
    let server2 = spawn_server(&config).await;

    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            server2.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let inst: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(inst["variables"]["payload"], JSON_ENVELOPE);
    assert_eq!(inst["variables"]["big"].as_str().unwrap().len(), 2048);

    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            server2.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let history: serde_json::Value = resp.json().await.unwrap();
    let started = history["events"]
        .as_array()
        .expect("events array")
        .iter()
        .find(|e| e["event_type"] == "ProcessStarted")
        .expect("ProcessStarted event")["payload"]
        .clone();
    assert!(
        started["initial_variables"]["big"]
            .as_str()
            .unwrap()
            .starts_with("__truncated:2048:"),
        "redacted history survives the restart"
    );
    assert_eq!(started["initial_variables"]["payload"], JSON_ENVELOPE);

    server2.stop().await;
}
