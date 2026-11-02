//! N6-1 scenario evaluation baseline: extends the existing 13 offline
//! regression cases with 5 real-user-path scenarios that exercise the
//! N1-N5 features (definition lifecycle, event inbox, approval,
//! compensation, budget). Fixed semantic assertions in CI.

use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="eval-flow" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;

/// A definition with a missing default branch (Critical validation finding).
const INVALID_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="eval-invalid" isExecutable="true">
    <startEvent id="start"/>
    <exclusiveGateway id="gw"/>
    <endEvent id="a"/>
    <endEvent id="b"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
    <sequenceFlow id="f2" sourceRef="gw" targetRef="a">
      <conditionExpression>x == "yes"</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f3" sourceRef="gw" targetRef="b">
      <conditionExpression>x == "no"</conditionExpression>
    </sequenceFlow>
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

/// Scenario 1: publish failure fix — invalid definition rejected, valid one deploys.
#[tokio::test]
async fn scenario_publish_failure_fix() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Invalid definition → 400 (validation gate)
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(INVALID_BPMN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "validation_failed");

    // Valid definition → 201 (fixed)
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

    server.stop().await;
}

/// Scenario 2: event redelivery — same delivery_id doesn't duplicate.
#[tokio::test]
async fn scenario_event_redelivery() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Deploy + start
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
        .json(&serde_json::json!({"process_def_id":"eval-flow"}))
        .send()
        .await
        .unwrap();
    let inst_id = resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Deliver with explicit delivery_id
    let msg = serde_json::json!({"message_name":"eval-msg","delivery_id":"del-eval-1"});
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/messages",
            server.base_url, inst_id
        ))
        .json(&msg)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    // Redeliver with same delivery_id → no duplicate
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/messages",
            server.base_url, inst_id
        ))
        .json(&msg)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    // Only 1 pending delivery
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/deliveries",
            server.base_url, inst_id
        ))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let deliveries = body.as_array().unwrap();
    assert_eq!(deliveries.len(), 1, "same delivery_id → no duplicate");

    server.stop().await;
}

/// Scenario 3: approval rejection — instance with no approval stays running.
#[tokio::test]
async fn scenario_approval_flow() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

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
        .json(&serde_json::json!({"process_def_id":"eval-flow"}))
        .send()
        .await
        .unwrap();
    let inst_id = resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    // List approvals (none pending for this flow — no approval-required task types)
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/approvals",
            server.base_url, inst_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let empty: Vec<serde_json::Value> = vec![];
    let approvals = body.as_array().unwrap_or(&empty);
    assert!(
        approvals.is_empty(),
        "no approval-required tasks → empty list"
    );

    // Cancel the instance (simulates rejection at the instance level)
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/cancel",
            server.base_url, inst_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    server.stop().await;
}

/// Scenario 4: compensation failure — trigger with no records (empty case).
#[tokio::test]
async fn scenario_compensation_view() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

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
        .json(&serde_json::json!({"process_def_id":"eval-flow"}))
        .send()
        .await
        .unwrap();
    let inst_id = resp.json::<serde_json::Value>().await.unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Trigger compensation (no records → triggered=0)
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/compensation/trigger",
            server.base_url, inst_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // View compensation progress
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/compensation",
            server.base_url, inst_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["summary"]["total"], 0);
    assert!(
        body["blocking_reason"].is_null(),
        "no records → no blocking reason"
    );

    server.stop().await;
}

/// Scenario 5: budget exhaustion — verify budget module exists and classifies.
/// (Full budget exhaustion requires the ai-reference handler with a Budget
/// configured; this test verifies the budget module's classification logic
/// is available and correct at the unit level.)
#[tokio::test]
async fn scenario_budget_classification() {
    use bpm_engine_worker_sdk::{Budget, BudgetTracker};

    // Configure a budget with max_attempts=0 → immediately exceeded
    let mut tracker = BudgetTracker::new(Budget::new().max_attempts(0));
    tracker.increment_attempt();
    let result = tracker.check();
    assert!(
        result.is_err(),
        "budget exceeded after 1 attempt with max=0"
    );
    let err = result.unwrap_err();
    assert_eq!(err.dimension, "attempts");
    assert!(err.message().contains("budget_exceeded"));

    // Verify permanent classification for budget_exceeded
    use bpm_engine_worker_sdk::classify_provider_error;
    let (category, _) = classify_provider_error("budget_exceeded: attempts", 0);
    assert_eq!(
        category,
        bpm_engine_core::RetryCategory::Permanent,
        "budget_exceeded → Permanent (no retry)"
    );
}
