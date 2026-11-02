//! N5-3 Agent operation recipes: machine-readable examples for the four
//! canonical agent workflows, verified against a real engine.

use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="recipe-flow" isExecutable="true">
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
        rate_limit: RateLimitConfig {
            requests_per_minute: 100_000,
        },
        ..Default::default()
    }
}

async fn spawn_server() -> TestServer {
    let app = build_app(&memory_config()).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let timer_cancel = app.timer_cancel.clone();
    let router = app.router.clone();
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestServer {
        base_url: format!("http://{}", addr),
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

#[tokio::test]
async fn recipe1_pre_publish_checks() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // 1. Validate (no side effects)
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/validate",
            server.base_url
        ))
        .json(&serde_json::json!({
            "format": "bpmn",
            "source": USER_TASK_BPMN,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], true);

    // 2. Deploy (validation gate)
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let def_id = body["process_definition_id"].as_str().unwrap();

    // 3. Audit trail
    let resp = client
        .get(format!(
            "{}/api/v1/process-definitions/{}/audit",
            server.base_url, def_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let records = body.as_array().expect("audit records array");
    assert!(!records.is_empty(), "audit trail has deploy record");
    assert_eq!(records[0]["operation"], "deploy");

    server.stop().await;
}

#[tokio::test]
async fn recipe3_message_delivery() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Deploy + start instance
    client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN)
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({"process_def_id":"recipe-flow"}))
        .send()
        .await
        .unwrap();
    let instance_id = resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Deliver a message
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/messages",
            server.base_url, instance_id
        ))
        .json(&serde_json::json!({"message_name":"test-msg","payload":{"key":"val"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "pending");
    assert!(body["delivery_id"].as_str().is_some());

    // Check pending deliveries
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/deliveries",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let deliveries = body.as_array().expect("deliveries array");
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["name"], "test-msg");

    // Broadcast a signal
    let resp = client
        .post(format!("{}/api/v1/signals", server.base_url))
        .json(&serde_json::json!({"signal_name":"test-signal"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    server.stop().await;
}

#[tokio::test]
async fn recipe4_fault_recovery() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Deploy + start instance
    client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN)
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({"process_def_id":"recipe-flow"}))
        .send()
        .await
        .unwrap();
    let instance_id = resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Trigger compensation (no records → triggered=0)
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/compensation/trigger",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // List compensation (empty but view works)
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/compensation",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["summary"]["total"], 0);

    server.stop().await;
}
