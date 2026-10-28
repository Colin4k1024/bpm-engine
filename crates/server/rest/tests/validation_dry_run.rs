//! Integration tests for E2-2 static validation and dry-run (issue #12).
//!
//! Memory mode via `build_app` + ephemeral port (E1-1 harness). Covers:
//! validate (BPMN + DSL) with unreachable nodes and clean docs, compile
//! errors returned as 200 data, dry-run routing / stuck / unresolved
//! terminus, malformed-envelope 4xx, and the acceptance proof that
//! validate + dry-run never deploy anything or create any instance, task,
//! or history record — while deploy keeps working afterwards.

use bpm_engine_server_rest::bpm_config::{BpmConfig, DatabaseConfig, ServerConfig};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Clean flow: start -> service task -> end. Validates with zero findings.
const CLEAN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="clean-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work">
      <extensionElements>
        <taskDefinition type="clean-work"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
    <sequenceFlow id="f2" sourceRef="work" targetRef="end"/>
  </process>
</definitions>"#;

/// Two-node island (isle1 <-> isle2) disconnected from the main flow:
/// passes the compiler (every node has incoming and outgoing flows) but is
/// unreachable from the start event and contains a cycle.
const UNREACHABLE_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="island-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work">
      <extensionElements>
        <taskDefinition type="island-main"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <serviceTask id="isle1">
      <extensionElements>
        <taskDefinition type="island-side"/>
      </extensionElements>
    </serviceTask>
    <serviceTask id="isle2">
      <extensionElements>
        <taskDefinition type="island-side"/>
      </extensionElements>
    </serviceTask>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
    <sequenceFlow id="f2" sourceRef="work" targetRef="end"/>
    <sequenceFlow id="f3" sourceRef="isle1" targetRef="isle2"/>
    <sequenceFlow id="f4" sourceRef="isle2" targetRef="isle1"/>
  </process>
</definitions>"#;

/// Exclusive gateway with a conditional branch and a default branch.
const GATEWAY_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="gateway-flow" isExecutable="true">
    <startEvent id="start"/>
    <exclusiveGateway id="gw" default="f_default"/>
    <serviceTask id="task_a">
      <extensionElements>
        <taskDefinition type="work-a"/>
      </extensionElements>
    </serviceTask>
    <serviceTask id="task_b">
      <extensionElements>
        <taskDefinition type="work-b"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
    <sequenceFlow id="f_a" sourceRef="gw" targetRef="task_a">
      <conditionExpression>x == "a"</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f_default" sourceRef="gw" targetRef="task_b"/>
    <sequenceFlow id="f3" sourceRef="task_a" targetRef="end"/>
    <sequenceFlow id="f4" sourceRef="task_b" targetRef="end"/>
  </process>
</definitions>"#;

/// Exclusive gateway with only conditional branches and no default: the
/// compiler accepts it, the static validator flags it, and a dry-run with
/// a non-matching variable gets stuck at the gateway.
const STUCK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="stuck-flow" isExecutable="true">
    <startEvent id="start"/>
    <exclusiveGateway id="gw"/>
    <endEvent id="end_a"/>
    <endEvent id="end_b"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
    <sequenceFlow id="f_a" sourceRef="gw" targetRef="end_a">
      <conditionExpression>x == "a"</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f_b" sourceRef="gw" targetRef="end_b">
      <conditionExpression>x == "b"</conditionExpression>
    </sequenceFlow>
  </process>
</definitions>"#;

/// Gateway guarded by expressions outside the dry-run evaluator's subset
/// (ordering comparisons): routing is reported as unresolved, not guessed.
const UNRESOLVED_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="unresolved-flow" isExecutable="true">
    <startEvent id="start"/>
    <exclusiveGateway id="gw"/>
    <endEvent id="end_big"/>
    <endEvent id="end_small"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
    <sequenceFlow id="f_big" sourceRef="gw" targetRef="end_big">
      <conditionExpression>amount &gt; 100</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f_small" sourceRef="gw" targetRef="end_small">
      <conditionExpression>amount &lt;= 100</conditionExpression>
    </sequenceFlow>
  </process>
</definitions>"#;

/// No end event: rejected by the compiler; the error must come back as
/// HTTP 200 data, not as an HTTP error.
const COMPILE_ERROR_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="broken-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work">
      <extensionElements>
        <taskDefinition type="broken-work"/>
      </extensionElements>
    </serviceTask>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
  </process>
</definitions>"#;

/// Clean cyclic parallel flow: fork -> both branches -> join -> loop back
/// to the fork's predecessor, with a conditional exit. The runtime re-arms
/// parallel joins per iteration (each fork execution creates a fresh join
/// group), so this flow is legal: the validator reports advisory warnings
/// only (`join_in_cycle` + `cycle_detected`), never a Critical finding.
const CYCLIC_PARALLEL_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="cyclic-parallel-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="pre">
      <extensionElements>
        <taskDefinition type="cycle-pre"/>
      </extensionElements>
    </serviceTask>
    <parallelGateway id="fork"/>
    <serviceTask id="work_a">
      <extensionElements>
        <taskDefinition type="cycle-a"/>
      </extensionElements>
    </serviceTask>
    <serviceTask id="work_b">
      <extensionElements>
        <taskDefinition type="cycle-b"/>
      </extensionElements>
    </serviceTask>
    <parallelGateway id="join"/>
    <exclusiveGateway id="gw" default="f_loop"/>
    <serviceTask id="post">
      <extensionElements>
        <taskDefinition type="cycle-post"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="pre"/>
    <sequenceFlow id="f2" sourceRef="pre" targetRef="fork"/>
    <sequenceFlow id="f3" sourceRef="fork" targetRef="work_a"/>
    <sequenceFlow id="f4" sourceRef="fork" targetRef="work_b"/>
    <sequenceFlow id="f5" sourceRef="work_a" targetRef="join"/>
    <sequenceFlow id="f6" sourceRef="work_b" targetRef="join"/>
    <sequenceFlow id="f7" sourceRef="join" targetRef="gw"/>
    <sequenceFlow id="f_exit" sourceRef="gw" targetRef="end">
      <conditionExpression>x == "exit"</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f_loop" sourceRef="gw" targetRef="post"/>
    <sequenceFlow id="f8" sourceRef="post" targetRef="pre"/>
  </process>
</definitions>"#;

const CLEAN_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "dsl-clean-flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "work", "type": "serviceTask", "task_type": "dsl-clean-work" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "work" },
    { "source": "work", "target": "end" }
  ]
}"#;

const UNREACHABLE_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "dsl-island-flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "work", "type": "serviceTask", "task_type": "dsl-main" },
    { "id": "end", "type": "end" },
    { "id": "isle1", "type": "serviceTask", "task_type": "dsl-isle" },
    { "id": "isle2", "type": "serviceTask", "task_type": "dsl-isle" }
  ],
  "edges": [
    { "source": "start", "target": "work" },
    { "source": "work", "target": "end" },
    { "source": "isle1", "target": "isle2" },
    { "source": "isle2", "target": "isle1" }
  ]
}"#;

const GATEWAY_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "dsl-gateway-flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "gw", "type": "exclusiveGateway" },
    { "id": "task_a", "type": "serviceTask", "task_type": "dsl-work-a" },
    { "id": "task_b", "type": "serviceTask", "task_type": "dsl-work-b" },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "gw" },
    { "source": "gw", "target": "task_a", "condition": { "kind": "variableEq", "variable": "x", "value": "a" } },
    { "source": "gw", "target": "task_b", "default": true },
    { "source": "task_a", "target": "end" },
    { "source": "task_b", "target": "end" }
  ]
}"#;

const COMPILE_ERROR_DSL: &str = r#"{
  "schema_version": 1,
  "process": { "id": "dsl-broken" },
  "nodes": [ { "id": "start", "type": "start", "taskTypo": 1 } ],
  "edges": []
}"#;

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

async fn post_source(
    client: &reqwest::Client,
    base_url: &str,
    endpoint: &str,
    format: &str,
    source: &str,
    variables: serde_json::Value,
) -> reqwest::Response {
    client
        .post(format!(
            "{}/api/v1/process-definitions/{}",
            base_url, endpoint
        ))
        .json(&serde_json::json!({
            "format": format,
            "source": source,
            "variables": variables,
        }))
        .send()
        .await
        .expect("request sent")
}

fn finding_codes(body: &serde_json::Value) -> Vec<String> {
    body["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|f| f["code"].as_str().expect("finding code").to_string())
        .collect()
}

fn step_node_ids(body: &serde_json::Value) -> Vec<String> {
    body["dry_run"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["node_id"].as_str().expect("step node id").to_string())
        .collect()
}

#[tokio::test]
async fn validate_bpmn_flags_unreachable_nodes() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "bpmn",
        UNREACHABLE_BPMN,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200, "validate must return 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], false, "unreachable nodes make it invalid");
    let codes = finding_codes(&body);
    assert_eq!(
        codes.iter().filter(|c| *c == "unreachable_node").count(),
        2,
        "both island nodes flagged: {:?}",
        codes
    );
    // Both findings are Critical with the island node ids.
    let unreachable: Vec<&serde_json::Value> = body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == "unreachable_node")
        .collect();
    assert!(unreachable.iter().all(|f| f["severity"] == "Critical"));
    let node_ids: Vec<&str> = unreachable
        .iter()
        .map(|f| f["node_id"].as_str().unwrap())
        .collect();
    assert!(node_ids.contains(&"isle1") && node_ids.contains(&"isle2"));
    // The island is a cycle too: one Warning, listing the members.
    let cycle: Vec<&serde_json::Value> = body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == "cycle_detected")
        .collect();
    assert_eq!(cycle.len(), 1);
    assert_eq!(cycle[0]["severity"], "Warning");
    assert!(cycle[0]["message"]
        .as_str()
        .unwrap()
        .contains("isle1 -> isle2"));

    server.stop().await;
}

#[tokio::test]
async fn validate_bpmn_clean_definition_is_valid() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "bpmn",
        CLEAN_BPMN,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], true, "clean doc must be valid: {:?}", body);
    assert!(
        body["findings"].as_array().unwrap().is_empty(),
        "no findings for a clean doc"
    );
    assert!(body.get("compile_errors").is_none());

    server.stop().await;
}

#[tokio::test]
async fn validate_bpmn_cyclic_parallel_flow_is_valid_with_warning() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "bpmn",
        CYCLIC_PARALLEL_BPMN,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // The runtime re-arms parallel joins per iteration (a fresh join group
    // per fork execution), so a clean cyclic parallel flow is legal: no
    // Critical findings, advisory warnings only.
    assert_eq!(
        body["valid"], true,
        "cyclic parallel flow must be valid: {:?}",
        body["findings"]
    );
    let join: Vec<&serde_json::Value> = body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == "join_in_cycle")
        .collect();
    assert_eq!(join.len(), 1, "findings: {:?}", body["findings"]);
    assert_eq!(join[0]["severity"], "Warning");
    assert_eq!(join[0]["node_id"], "join");
    assert!(
        body["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "cycle_detected"),
        "the cycle itself is reported as a Warning: {:?}",
        body["findings"]
    );
    assert!(
        body["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["severity"] != "Critical"),
        "no Critical finding for a clean cyclic parallel flow: {:?}",
        body["findings"]
    );

    server.stop().await;
}

#[tokio::test]
async fn validate_dsl_flags_unreachable_nodes() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "dsl",
        UNREACHABLE_DSL,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], false);
    let codes = finding_codes(&body);
    assert!(
        codes.iter().filter(|c| *c == "unreachable_node").count() == 2,
        "dsl island flagged: {:?}",
        codes
    );

    // Clean DSL document is valid.
    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "dsl",
        CLEAN_DSL,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], true, "clean DSL must be valid: {:?}", body);
    assert!(body["findings"].as_array().unwrap().is_empty());

    server.stop().await;
}

#[tokio::test]
async fn validate_returns_compile_errors_as_data() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    // BPMN: compile errors are HTTP 200 data, not HTTP errors.
    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "bpmn",
        COMPILE_ERROR_BPMN,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "compile errors are data, not HTTP errors"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], false);
    let errors = body["compile_errors"].as_array().expect("compile_errors");
    assert!(!errors.is_empty());
    let codes: Vec<&str> = errors.iter().map(|e| e["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"NoEndEvent"), "codes: {:?}", codes);

    // DSL: same contract, snake_case codes with JSON-pointer paths.
    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "dsl",
        COMPILE_ERROR_DSL,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], false);
    let errors = body["compile_errors"].as_array().expect("compile_errors");
    assert_eq!(errors[0]["code"], "unknown_field");
    assert_eq!(errors[0]["path"], "/nodes/0/taskTypo");

    // Unparseable BPMN XML is a parse error (single message object).
    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "bpmn",
        "<not-bpmn",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], false);
    assert!(
        body["compile_errors"]["error"].as_str().is_some(),
        "parse error object: {:?}",
        body["compile_errors"]
    );

    server.stop().await;
}

#[tokio::test]
async fn dry_run_bpmn_routes_gateway_with_variables() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    // Matching variable: routed through task_a.
    let resp = post_source(
        &client,
        &server.base_url,
        "dry-run",
        "bpmn",
        GATEWAY_BPMN,
        serde_json::json!({ "x": "a" }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Warnings/info do not invalidate: only the Info condition variable
    // contract for `x` is reported, so the definition stays valid.
    assert_eq!(body["valid"], true, "findings: {:?}", body["findings"]);
    let codes = finding_codes(&body);
    assert!(codes.iter().any(|c| c == "condition_variable_scope"));
    assert_eq!(
        step_node_ids(&body),
        vec!["start", "gw", "task_a", "end"],
        "routed path must follow the matching condition"
    );
    // External task step carries the task type and is assumed complete.
    let task_step = &body["dry_run"]["steps"][2];
    assert_eq!(task_step["node_type"], "externalTask");
    assert_eq!(task_step["detail"], "work-a");
    assert_eq!(body["dry_run"]["terminus"]["type"], "completedAt");
    assert_eq!(body["dry_run"]["terminus"]["node_id"], "end");
    assert_eq!(body["dry_run"]["max_steps"], 100);

    // Non-matching variable: default branch.
    let resp = post_source(
        &client,
        &server.base_url,
        "dry-run",
        "bpmn",
        GATEWAY_BPMN,
        serde_json::json!({ "x": "zzz" }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        step_node_ids(&body),
        vec!["start", "gw", "task_b", "end"],
        "default branch must be taken when no condition matches"
    );
    assert_eq!(body["dry_run"]["steps"][2]["detail"], "work-b");

    server.stop().await;
}

#[tokio::test]
async fn dry_run_dsl_routes_gateway_with_variables() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let resp = post_source(
        &client,
        &server.base_url,
        "dry-run",
        "dsl",
        GATEWAY_DSL,
        serde_json::json!({ "x": "a" }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["valid"], true);
    assert_eq!(
        step_node_ids(&body),
        vec!["start", "gw", "task_a", "end"],
        "dsl variableEq routes the same as BPMN"
    );
    assert_eq!(body["dry_run"]["steps"][2]["detail"], "dsl-work-a");
    assert_eq!(body["dry_run"]["terminus"]["type"], "completedAt");

    server.stop().await;
}

#[tokio::test]
async fn dry_run_stuck_flow_reports_stuck_at() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let resp = post_source(
        &client,
        &server.base_url,
        "dry-run",
        "bpmn",
        STUCK_BPMN,
        serde_json::json!({ "x": "c" }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Static finding: no default branch.
    assert_eq!(body["valid"], false);
    let codes = finding_codes(&body);
    assert!(codes.iter().any(|c| c == "missing_default_branch"));
    // Walk: stuck at the gateway, never reaches an end event.
    assert_eq!(step_node_ids(&body), vec!["start", "gw"]);
    assert_eq!(body["dry_run"]["terminus"]["type"], "stuckAt");
    assert_eq!(body["dry_run"]["terminus"]["node_id"], "gw");
    assert!(body["dry_run"]["terminus"]["reason"]
        .as_str()
        .unwrap()
        .contains("no default branch"));

    server.stop().await;
}

#[tokio::test]
async fn dry_run_unsupported_expression_reports_unresolved_branch() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    let resp = post_source(
        &client,
        &server.base_url,
        "dry-run",
        "bpmn",
        UNRESOLVED_BPMN,
        serde_json::json!({ "amount": "500" }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["dry_run"]["terminus"]["type"], "unresolvedBranch");
    assert_eq!(body["dry_run"]["terminus"]["node_id"], "gw");
    let unresolved: Vec<&str> = body["dry_run"]["unresolved_conditions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(
        unresolved.contains(&"amount > 100") && unresolved.contains(&"amount <= 100"),
        "unresolved conditions: {:?}",
        unresolved
    );

    server.stop().await;
}

#[tokio::test]
async fn validate_and_dry_run_have_no_side_effects() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    // Exercise both endpoints with both formats, success and failure cases.
    for (endpoint, format, source, variables) in [
        ("validate", "bpmn", UNREACHABLE_BPMN, serde_json::json!({})),
        ("validate", "bpmn", CLEAN_BPMN, serde_json::json!({})),
        (
            "validate",
            "bpmn",
            COMPILE_ERROR_BPMN,
            serde_json::json!({}),
        ),
        ("validate", "dsl", UNREACHABLE_DSL, serde_json::json!({})),
        ("validate", "dsl", CLEAN_DSL, serde_json::json!({})),
        ("validate", "dsl", COMPILE_ERROR_DSL, serde_json::json!({})),
        (
            "dry-run",
            "bpmn",
            GATEWAY_BPMN,
            serde_json::json!({ "x": "a" }),
        ),
        (
            "dry-run",
            "bpmn",
            STUCK_BPMN,
            serde_json::json!({ "x": "c" }),
        ),
        (
            "dry-run",
            "dsl",
            GATEWAY_DSL,
            serde_json::json!({ "x": "a" }),
        ),
    ] {
        let resp = post_source(
            &client,
            &server.base_url,
            endpoint,
            format,
            source,
            variables,
        )
        .await;
        assert_eq!(
            resp.status(),
            200,
            "{} {} must return 200",
            endpoint,
            format
        );
    }

    // Acceptance: dry-run 不产生实例 — no instances.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["instances"].as_array().unwrap().is_empty(),
        "no process instances may exist after validate/dry-run"
    );

    // No tasks either.
    let resp = client
        .get(format!("{}/api/v1/tasks", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.as_array().unwrap().is_empty(),
        "no tasks may exist after validate/dry-run"
    );

    // No definition was deployed: only the bootstrap definitions exist.
    let resp = client
        .get(format!("{}/api/v1/process-definitions", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let keys: Vec<String> = body["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap().to_string())
        .collect();
    for bootstrap in ["minimal", "payment-flow"] {
        assert!(
            keys.contains(&bootstrap.to_string()),
            "bootstrap definition '{}' must still be there: {:?}",
            bootstrap,
            keys
        );
    }
    for validated in [
        "island-flow",
        "clean-flow",
        "broken-flow",
        "gateway-flow",
        "stuck-flow",
        "unresolved-flow",
        "dsl-clean-flow",
        "dsl-island-flow",
        "dsl-gateway-flow",
        "dsl-broken",
    ] {
        assert!(
            !keys.contains(&validated.to_string()),
            "validated source '{}' must NOT be deployed: {:?}",
            validated,
            keys
        );
    }

    // Deploy still works normally afterwards.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body(CLEAN_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "deploy must keep working");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["process_definition_id"], "clean-flow");

    // And the deployed definition is usable: start an instance.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({ "process_def_id": "clean-flow" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["instances"].as_array().unwrap().len(),
        1,
        "positive control: a started instance is visible"
    );

    server.stop().await;
}

#[tokio::test]
async fn validate_and_dry_run_reject_malformed_envelopes() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();

    // Bad format value.
    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "yaml",
        CLEAN_BPMN,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_format");

    let resp = post_source(
        &client,
        &server.base_url,
        "dry-run",
        "yaml",
        CLEAN_DSL,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_format");

    // Empty source.
    let resp = post_source(
        &client,
        &server.base_url,
        "validate",
        "bpmn",
        "   ",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "empty_source");

    // Unparseable request body.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/dry-run",
            server.base_url
        ))
        .body("not json at all")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_request_body");

    server.stop().await;
}
