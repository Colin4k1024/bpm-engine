//! REST-level tests for the E2-4 API contract: idempotency keys,
//! pagination, stable error codes, and the typed JSON-body extractor.
//!
//! Memory driver — runs in the default CI job. Reuses the E1-1 harness
//! pattern (`build_app` + ephemeral port + real HTTP).

use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// BPMN with a user task so started instances stay RUNNING.
const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="idem-probe" isExecutable="true">
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
        .expect("failed to bind ephemeral port");
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

async fn deploy(client: &reqwest::Client, server: &TestServer) {
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
}

async fn start(
    client: &reqwest::Client,
    server: &TestServer,
    idempotency_key: Option<&str>,
    tenant: Option<&str>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut req = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .json(&serde_json::json!({ "process_def_id": "idem-probe" }));
    if let Some(key) = idempotency_key {
        req = req.header("Idempotency-Key", key);
    }
    if let Some(tenant) = tenant {
        req = req.header("X-Tenant-Id", tenant);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

async fn instance_count(client: &reqwest::Client, server: &TestServer) -> usize {
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .query(&[("limit", "500")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["instances"].as_array().unwrap().len()
}

// --- Idempotency (E2-4) -----------------------------------------------------

#[tokio::test]
async fn same_idempotency_key_replays_one_instance() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    // First request with the key: creates (201).
    let (status, body) = start(&client, &server, Some("order-42"), None).await;
    assert_eq!(status, 201, "first keyed start creates: {body}");
    let first_id = body["instance_id"].as_str().unwrap().to_string();
    assert_eq!(body["status"], "RUNNING");

    // Same key again: 200 replay with the SAME instance id.
    let (status, body) = start(&client, &server, Some("order-42"), None).await;
    assert_eq!(status, 200, "second keyed start replays: {body}");
    assert_eq!(body["instance_id"].as_str().unwrap(), first_id);
    assert_eq!(body["status"], "RUNNING");

    // Exactly ONE instance exists (no duplicate creation).
    assert_eq!(instance_count(&client, &server).await, 1);

    // A different key creates a second instance.
    let (status, _body) = start(&client, &server, Some("order-43"), None).await;
    assert_eq!(status, 201);
    assert_eq!(instance_count(&client, &server).await, 2);

    server.stop().await;
}

#[tokio::test]
async fn idempotency_keys_are_tenant_scoped() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    let (status, body_a) = start(&client, &server, Some("shared-key"), Some("tenant-a")).await;
    assert_eq!(status, 201);
    let id_a = body_a["instance_id"].as_str().unwrap().to_string();

    // The SAME key under a DIFFERENT tenant creates a NEW instance.
    let (status, body_b) = start(&client, &server, Some("shared-key"), Some("tenant-b")).await;
    assert_eq!(status, 201, "same key, other tenant must create: {body_b}");
    let id_b = body_b["instance_id"].as_str().unwrap().to_string();
    assert_ne!(id_a, id_b);

    // Replaying each tenant's key returns its own instance.
    let (status, body) = start(&client, &server, Some("shared-key"), Some("tenant-a")).await;
    assert_eq!(status, 200);
    assert_eq!(body["instance_id"].as_str().unwrap(), id_a);
    let (status, body) = start(&client, &server, Some("shared-key"), Some("tenant-b")).await;
    assert_eq!(status, 200);
    assert_eq!(body["instance_id"].as_str().unwrap(), id_b);

    server.stop().await;
}

#[tokio::test]
async fn malformed_idempotency_keys_are_rejected() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    // Oversized key (129 printable ASCII bytes).
    let oversized = "x".repeat(129);
    let (status, body) = start(&client, &server, Some(&oversized), None).await;
    assert_eq!(status, 400, "oversized key must be rejected: {body}");
    assert_eq!(body["code"], "invalid_request");

    // Empty key: present but below the 1-byte minimum.
    let (status, body) = start(&client, &server, Some(""), None).await;
    assert_eq!(status, 400, "empty key must be rejected: {body}");
    assert_eq!(body["code"], "invalid_request");

    // No instance was created by the rejected requests.
    assert_eq!(instance_count(&client, &server).await, 0);

    server.stop().await;
}

/// E2-4 review fix: the tenant id is bounded ONLY at the idempotency save
/// boundary (the (tenant, key) pair co-forms the primary key) — a key-less
/// start with the same oversized tenant id is unaffected.
#[tokio::test]
async fn oversized_tenant_id_is_rejected_only_with_an_idempotency_key() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    // 129 printable ASCII bytes — one over the 128-byte bound.
    let oversized_tenant = "t".repeat(129);

    // WITH an idempotency key: rejected before anything is stored.
    let (status, body) = start(&client, &server, Some("order-1"), Some(&oversized_tenant)).await;
    assert_eq!(
        status, 400,
        "oversized tenant + idempotency key must be rejected: {body}"
    );
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("tenant id too long"),
        "the rejection names the tenant bound: {body}"
    );
    assert_eq!(instance_count(&client, &server).await, 0);

    // Without a key: the SAME tenant id is accepted (unbounded semantics
    // are preserved on every other path).
    let (status, body) = start(&client, &server, None, Some(&oversized_tenant)).await;
    assert_eq!(
        status, 201,
        "key-less start ignores the tenant bound: {body}"
    );
    assert_eq!(instance_count(&client, &server).await, 1);

    // 128 bytes (exactly at the bound) + key: still valid.
    let bounded_tenant = "t".repeat(128);
    let (status, body) = start(&client, &server, Some("order-2"), Some(&bounded_tenant)).await;
    assert_eq!(
        status, 201,
        "128-byte tenant with a key is accepted: {body}"
    );

    server.stop().await;
}

// --- Pagination (E2-4) ------------------------------------------------------

#[tokio::test]
async fn instance_list_pagination_windows_and_validates() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    // 5 instances (ids sort lexicographically as uuids; only count + windows matter).
    for i in 0..5 {
        let (status, _body) = start(&client, &server, Some(&format!("page-{i}")), None).await;
        assert_eq!(status, 201);
    }

    // Explicit window.
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .query(&[("limit", "2"), ("offset", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["instances"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["limit"], 2);
    assert_eq!(body["pagination"]["offset"], 1);
    assert_eq!(body["pagination"]["returned"], 2);

    // Default: limit 50, offset 0 — all 5 returned.
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["instances"].as_array().unwrap().len(), 5);
    assert_eq!(body["pagination"]["limit"], 50);
    assert_eq!(body["pagination"]["offset"], 0);
    assert_eq!(body["pagination"]["returned"], 5);

    // Offset beyond the end: empty page, returned 0.
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .query(&[("offset", "100")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["instances"].as_array().unwrap().len(), 0);
    assert_eq!(body["pagination"]["returned"], 0);

    // Out-of-range / malformed limits: 400 invalid_request.
    for bad in ["0", "501", "abc", "-1"] {
        let resp = client
            .get(format!("{}/api/v1/process-instances", server.base_url))
            .query(&[("limit", bad)])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "limit={bad:?} must be rejected");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"], "invalid_request", "limit={bad:?}");
    }

    server.stop().await;
}

#[tokio::test]
async fn task_list_pagination_slices_in_memory() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    // Three instances, each waiting at the same user task.
    for i in 0..3 {
        let (status, _) = start(&client, &server, Some(&format!("tasks-{i}")), None).await;
        assert_eq!(status, 201);
    }

    // Without params: full list (previous behavior).
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/tasks", server.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.as_array().unwrap().len(), 3);

    // With a window: exactly the slice.
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/tasks", server.base_url))
        .query(&[("limit", "2"), ("offset", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);

    // Invalid limit on the route-level endpoint too.
    let resp = client
        .get(format!("{}/api/v1/tasks", server.base_url))
        .query(&[("limit", "0")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_request");

    server.stop().await;
}

#[tokio::test]
async fn history_limit_truncates_after_sorting() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    let (status, body) = start(&client, &server, Some("hist-1"), None).await;
    assert_eq!(status, 201);
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    // Unfiltered: the full event list.
    let full: serde_json::Value = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            server.base_url, instance_id
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let total = full["events"].as_array().unwrap().len();
    assert!(total >= 2, "a started instance has at least two events");

    // limit=1: the first event only (truncation after sorting).
    let limited: serde_json::Value = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            server.base_url, instance_id
        ))
        .query(&[("limit", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let events = limited["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0], full["events"][0],
        "limit must keep the FIRST events in causal order"
    );

    // Invalid limit: 400 invalid_request.
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            server.base_url, instance_id
        ))
        .query(&[("limit", "0")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_request");

    server.stop().await;
}

// --- Typed error contract (E2-4) --------------------------------------------

#[tokio::test]
async fn not_found_errors_carry_the_stable_code() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/00000000-0000-4000-8000-000000000000",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "not_found");
    assert!(body["error"].as_str().unwrap().contains("not found"));

    let resp = client
        .get(format!(
            "{}/api/v1/process-definitions/does-not-exist",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "not_found");

    let resp = client
        .get(format!(
            "{}/api/v1/dead-letters/00000000-0000-4000-8000-000000000000",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "not_found");

    let resp = client
        .get(format!(
            "{}/api/v1/replay/00000000-0000-4000-8000-000000000000/snapshot",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "not_found");

    server.stop().await;
}

#[tokio::test]
async fn invalid_json_bodies_return_the_typed_error() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server).await;

    // Syntax error: 400 invalid_json.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("content-type", "application/json")
        .body(r#"{ "process_def_id": "#) // truncated JSON
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "syntax errors are 400");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_json");
    assert!(body["error"].as_str().unwrap().len() > 10);

    // Well-formed JSON that does not match the target type: 422 invalid_json.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("content-type", "application/json")
        .body(r#"{ "variables": {} }"#) // process_def_id missing
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422, "type mismatches are 422");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_json");

    // Missing content type on a JSON endpoint: 400 invalid_json (not the
    // old plain-text axum rejection).
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .body(r#"{"worker_id": "w1"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_json");

    server.stop().await;
}

#[tokio::test]
async fn extend_lock_conflict_carries_the_lock_conflict_code() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Unknown task: the store refuses the extension → 409 lock_conflict.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/00000000-0000-4000-8000-000000000000/extend-lock",
            server.base_url
        ))
        .json(&serde_json::json!({ "worker_id": "w1", "extension_ms": 1000 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "lock_conflict");

    server.stop().await;
}

#[tokio::test]
async fn deploy_parse_error_shape_is_unchanged() {
    // The memory deployer cannot fail, so the 500 `deploy_failed` variant
    // is covered by the openapi_contract suite (spec enum + DeployFailed
    // response); here we pin the UNCHANGED 400 parse shape so the new
    // Persist variant cannot regress it: parse errors keep the legacy
    // `{error}` body with NO code field.
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("content-type", "application/xml")
        .body("<not-bpmn")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().is_some(),
        "parse errors keep the error field"
    );
    assert!(body["code"].is_null(), "parse errors carry no code field");

    server.stop().await;
}
