//! Integration tests for the memory storage backend (default driver).
//!
//! Builds the full app via `build_app`, serves the router on an ephemeral
//! port, and exercises the REST API over real HTTP. Memory mode must keep
//! working exactly as before the storage assembly refactor: bootstrap
//! definitions (`minimal`, `payment-flow`), BPMN deploy, instance start,
//! and invariant checks.

use bpm_engine_server_rest::bpm_config::{BpmConfig, DatabaseConfig, ServerConfig};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// BPMN with a UserTask so the started instance blocks (stays Running).
const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="restart-probe" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;

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
        // Give background tasks a moment to observe the cancellation.
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

#[tokio::test]
async fn memory_mode_full_api_roundtrip() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    // Liveness probe
    let resp = client
        .get(format!("{}/health", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");

    // Deploy a BPMN definition with a user task
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
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["process_definition_id"], "restart-probe");

    // Start an instance — it blocks at the user task
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({ "process_def_id": "restart-probe" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "start should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"]
        .as_str()
        .expect("instance_id in response")
        .to_string();

    // Get the instance — Running, waiting at the user task node
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "RUNNING");
    assert_eq!(body["process_def_id"], "restart-probe");
    let current_nodes: Vec<String> = body["current_nodes"]
        .as_array()
        .expect("current_nodes array")
        .iter()
        .map(|v| v.as_str().expect("node id").to_string())
        .collect();
    assert!(
        current_nodes.contains(&"review".to_string()),
        "expected waiting node 'review', got {:?}",
        current_nodes
    );

    // Definitions list contains the deployed def AND the bootstrap defs
    let resp = client
        .get(format!("{}/api/v1/process-definitions", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let keys: Vec<String> = body["definitions"]
        .as_array()
        .expect("definitions array")
        .iter()
        .map(|d| d["key"].as_str().expect("definition key").to_string())
        .collect();
    for expected in ["restart-probe", "minimal", "payment-flow"] {
        assert!(
            keys.contains(&expected.to_string()),
            "definitions missing '{}': {:?}",
            expected,
            keys
        );
    }

    // Invariant check returns 200. Note: the engine creates instances with
    // version 0, which the checker reports as an Info-level finding (pre-
    // existing behavior); assert there is nothing worse than Info.
    let resp = client
        .get(format!("{}/api/v1/invariants/check", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let severe: Vec<&str> = body["violations"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .filter(|v| v["severity"] != "Info")
                .map(|v| v["description"].as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        severe.is_empty(),
        "invariants should have no Warning/Critical violations: {:?}",
        severe
    );

    // Instance list includes the running instance
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<String> = body["instances"]
        .as_array()
        .expect("instances array")
        .iter()
        .map(|i| i["instance_id"].as_str().expect("instance id").to_string())
        .collect();
    assert!(ids.contains(&instance_id));

    server.stop().await;
}

#[tokio::test]
async fn memory_mode_minimal_def_completes_instantly() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    // The bootstrap "minimal" def (start -> end) completes during the call.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({ "process_def_id": "minimal" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"]
        .as_str()
        .expect("instance_id in response")
        .to_string();

    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "COMPLETED");

    server.stop().await;
}

#[tokio::test]
async fn build_app_rejects_unknown_driver() {
    let mut config = memory_config();
    config.database.driver = "sqlite".to_string();
    let err = build_app(&config)
        .await
        .err()
        .expect("build_app should reject an unknown driver");
    assert!(
        err.to_string().contains("unsupported database.driver"),
        "unexpected error: {}",
        err
    );
}

#[tokio::test]
async fn build_app_rejects_postgres_without_url() {
    let mut config = memory_config();
    config.database.driver = "postgres".to_string();
    config.database.url = String::new();
    let err = build_app(&config)
        .await
        .err()
        .expect("build_app should reject postgres without a url");
    assert!(
        err.to_string().contains("database.url must not be empty"),
        "unexpected error: {}",
        err
    );
}
