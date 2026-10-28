//! REST-level tests for JWT identity, RBAC, and cross-tenant isolation
//! (E5-1, issue #23).
//!
//! The tests configure `auth.jwt_secret` (AND `auth.api_key`), which makes
//! `build_app` set `BPM_JWT_SECRET` / `BPM_API_KEY` for the whole test
//! PROCESS — every test in this file therefore runs with JWT auth enabled
//! and the same fixed credentials (tests in other files run in their own
//! processes and are unaffected; the OPEN-mode behavior is covered by the
//! pre-existing suites, which run without either variable).
//!
//! Covered:
//! - JWT enforcement: missing/garbage/expired/wrong-secret bearer tokens
//!   answer 401 `invalid_token`; the malformed non-bearer Authorization
//!   header too; a valid token authenticates.
//! - The role matrix (E5-1): each role × representative endpoints, allowed
//!   AND denied (403 `forbidden`).
//! - Cross-tenant isolation: tenant-A tokens cannot read (404
//!   `not_found`, byte-equal to the unknown-id body), complete, or replay
//!   tenant-B instances; listings are claim-scoped; worker
//!   fetch-and-lock delivers (and leases) only the claim tenant's tasks.
//! - The X-Tenant-Id deprecation: a header CONFLICTING with the claim is
//!   403 `forbidden`; an agreeing or absent header is accepted.
//! - The admin API-key bootstrap path: full access, X-Tenant-Id honored,
//!   JWT takes precedence when a bearer token is present.
//!
//! Memory driver — no Docker needed.

use bpm_engine_server_rest::bpm_config::{
    AuthConfig, BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use bpm_engine_server_rest::middleware::mint_jwt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// Fixed signing secret for the whole test binary (see the module docs).
const TEST_SECRET: &str = "rbac-test-secret";
/// Fixed admin API key (the bootstrap credential under test).
const TEST_API_KEY: &str = "rbac-test-admin-key";

const TENANT_A: &str = "acme";
const TENANT_B: &str = "globex";

/// A BPMN with a user task (deployed by the admin/deployer in tests).
const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="rbac-probe" isExecutable="true">
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
            api_key: TEST_API_KEY.to_string(),
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

/// Now + one hour (unix seconds): the default token expiry.
fn soon() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

/// Mint a TEST_SECRET-signed token for `role` @ `tenant`.
fn token(role: &str, tenant: &str) -> String {
    mint_jwt(TEST_SECRET, "test-actor", tenant, role, soon()).expect("mint token")
}

/// The `Authorization` header value authenticating as `role` @ `tenant`.
fn bearer(role: &str, tenant: &str) -> String {
    format!("Bearer {}", token(role, tenant))
}

/// Deploy `USER_TASK_BPMN` with the admin API key; returns the definition id.
async fn deploy_probe(client: &reqwest::Client, server: &TestServer) -> String {
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("X-API-Key", TEST_API_KEY)
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "admin deploy should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["process_definition_id"].as_str().unwrap().to_string()
}

/// Start an instance of `def` authenticated as `role` @ `tenant`.
async fn start_instance(
    client: &reqwest::Client,
    server: &TestServer,
    def: &str,
    role: &str,
    tenant: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer(role, tenant))
        .json(&serde_json::json!({ "process_def_id": def }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

/// GET an endpoint authenticated as `role` @ `tenant`.
async fn get(
    client: &reqwest::Client,
    server: &TestServer,
    path: &str,
    role: &str,
    tenant: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .get(format!("{}{}", server.base_url, path))
        .header("Authorization", bearer(role, tenant))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

/// Assert a typed error response: `(status, code)`.
async fn assert_error(
    resp: reqwest::Response,
    expected_status: u16,
    expected_code: &str,
) -> serde_json::Value {
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        status.as_u16(),
        expected_status,
        "expected {} {}, got {} {}: {body}",
        expected_status,
        expected_code,
        status,
        body["code"].as_str().unwrap_or("?")
    );
    assert_eq!(
        body["code"], expected_code,
        "expected code {expected_code:?}, got {}: {body}",
        body["code"]
    );
    body
}

// --- JWT enforcement ---------------------------------------------------------

#[tokio::test]
async fn jwt_is_required_and_rejections_carry_invalid_token() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // No credentials at all.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "invalid_token").await;

    // Garbage bearer token.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", "Bearer not-a-jwt")
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "invalid_token").await;

    // Token signed with the WRONG secret.
    let wrong_secret = mint_jwt("other-secret", "a", TENANT_A, "operator", soon()).expect("mint");
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", format!("Bearer {wrong_secret}"))
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "invalid_token").await;

    // Expired token (strict expiry, zero leeway).
    let expired = mint_jwt(TEST_SECRET, "a", TENANT_A, "operator", 1).expect("mint");
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", format!("Bearer {expired}"))
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "invalid_token").await;

    // Non-bearer Authorization header under JWT enforcement.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", "Basic dXNlcjpwYXNz")
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "invalid_token").await;

    // A wrong API key on the fallback path keeps its legacy code.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", "wrong-key")
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "unauthorized").await;

    // A valid token authenticates.
    let (status, _body) = get(
        &client,
        &server,
        "/api/v1/process-instances",
        "auditor",
        TENANT_A,
    )
    .await;
    assert_eq!(status, 200);

    // Unauthenticated top-level routes stay open.
    let resp = client
        .get(format!("{}/health", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .get(format!("{}/capabilities", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    server.stop().await;
}

// --- Role matrix -------------------------------------------------------------

#[tokio::test]
async fn deployer_role_matrix() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Reads allowed.
    let (status, _body) = get(
        &client,
        &server,
        "/api/v1/process-definitions",
        "deployer",
        TENANT_A,
    )
    .await;
    assert_eq!(status, 200);

    // Deploy / activate / deprecate allowed.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("Authorization", bearer("deployer", TENANT_A))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "deployer may deploy");
    let body: serde_json::Value = resp.json().await.unwrap();
    let def_id = body["process_definition_id"].as_str().unwrap().to_string();

    let resp = client
        .put(format!(
            "{}/api/v1/process-definitions/{}/activate",
            server.base_url, def_id
        ))
        .header("Authorization", bearer("deployer", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "deployer may activate");

    let resp = client
        .put(format!(
            "{}/api/v1/process-definitions/{}/deprecate",
            server.base_url, def_id
        ))
        .header("Authorization", bearer("deployer", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "deployer may deprecate");

    // Side-effect-free POSTs allowed for every role.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/validate",
            server.base_url
        ))
        .header("Authorization", bearer("deployer", TENANT_A))
        .json(&serde_json::json!({ "format": "bpmn", "source": USER_TASK_BPMN }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "deployer may validate");

    // Instance / task / worker / DLQ operations denied.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("deployer", TENANT_A))
        .json(&serde_json::json!({ "process_def_id": "minimal" }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("deployer", TENANT_A))
        .json(&serde_json::json!({
            "worker_id": "w", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/tasks/some:review/complete",
            server.base_url
        ))
        .header("Authorization", bearer("deployer", TENANT_A))
        .json(&serde_json::json!({ "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/dead-letters/00000000-0000-4000-8000-000000000000/requeue",
            server.base_url
        ))
        .header("Authorization", bearer("deployer", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    server.stop().await;
}

#[tokio::test]
async fn operator_role_matrix() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    // The deployer (admin) deploys; the operator operates.
    let def_id = deploy_probe(&client, &server).await;

    // Reads allowed.
    let (status, _) = get(
        &client,
        &server,
        "/api/v1/process-instances",
        "operator",
        TENANT_A,
    )
    .await;
    assert_eq!(status, 200);

    // Start instance allowed (the bootstrap `minimal` definition).
    let (status, body) = start_instance(&client, &server, "minimal", "operator", TENANT_A).await;
    assert_eq!(status, 201, "operator may start instances: {body}");

    // Complete a user task allowed.
    let (status, body) = start_instance(&client, &server, &def_id, "operator", TENANT_A).await;
    assert_eq!(status, 201);
    let instance_id = body["instance_id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{}:review/complete",
            server.base_url, instance_id
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .json(&serde_json::json!({ "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "operator may complete user tasks");

    // Replay allowed.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/replay",
            server.base_url, instance_id
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "operator may create replay sessions");

    // Deploy / activate denied.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .put(format!(
            "{}/api/v1/process-definitions/{}/activate",
            server.base_url, def_id
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    // Worker lease operations denied (worker-only by design).
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .json(&serde_json::json!({
            "worker_id": "w", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    // Side-effect-free POSTs allowed.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/dry-run",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .json(&serde_json::json!({ "format": "bpmn", "source": USER_TASK_BPMN }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "operator may dry-run");

    server.stop().await;
}

#[tokio::test]
async fn worker_role_matrix() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    // An operator of tenant A starts the bootstrap payment-flow (its
    // ExternalTask node creates a `payment` task for the worker).
    let (status, body) =
        start_instance(&client, &server, "payment-flow", "operator", TENANT_A).await;
    assert_eq!(status, 201, "operator starts payment-flow: {body}");

    // Fetch-and-lock allowed — scoped to the claim tenant.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({
            "worker_id": "w-a", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "worker may fetch-and-lock");
    let tasks: serde_json::Value = resp.json().await.unwrap();
    let task = tasks[0].as_object().expect("one payment task").clone();
    let task_id = task["task_id"].as_str().unwrap().to_string();

    // Complete allowed.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{}/complete",
            server.base_url, task_id
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({ "worker_id": "w-a", "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "worker may complete external tasks");

    // Reads allowed.
    let (status, _) = get(&client, &server, "/api/v1/tasks", "worker", TENANT_A).await;
    assert_eq!(status, 200);

    // Instance / deploy / user-task operations denied.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({ "process_def_id": "minimal" }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/tasks/x:review/complete",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({ "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    // Side-effect-free POSTs allowed.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/validate",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({ "format": "bpmn", "source": USER_TASK_BPMN }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "worker may validate");

    server.stop().await;
}

#[tokio::test]
async fn auditor_role_matrix() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def_id = deploy_probe(&client, &server).await;
    let (status, body) = start_instance(&client, &server, &def_id, "operator", TENANT_A).await;
    assert_eq!(status, 201);
    let instance_id = body["instance_id"].as_str().unwrap().to_string();

    // All reads allowed (incl. history/steps/trace/invariants/capabilities).
    for path in [
        "/api/v1/process-instances",
        "/api/v1/process-instances?limit=10",
        &format!("/api/v1/process-instances/{instance_id}"),
        &format!("/api/v1/process-instances/{instance_id}/history"),
        &format!("/api/v1/process-instances/{instance_id}/steps"),
        &format!("/api/v1/process-instances/{instance_id}/trace"),
        "/api/v1/process-definitions",
        &format!("/api/v1/process-definitions/{def_id}"),
        "/api/v1/process-definitions/versions/rbac-probe",
        "/api/v1/tasks",
        "/api/v1/dead-letters",
        "/api/v1/invariants/check",
    ] {
        let (status, body) = get(&client, &server, path, "auditor", TENANT_A).await;
        assert_eq!(status, 200, "auditor may GET {path}: {body}");
    }

    // Every mutating class denied.
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("auditor", TENANT_A))
        .json(&serde_json::json!({ "process_def_id": "minimal" }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("Authorization", bearer("auditor", TENANT_A))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{}/replay",
            server.base_url, instance_id
        ))
        .header("Authorization", bearer("auditor", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/tasks/x:review/complete",
            server.base_url
        ))
        .header("Authorization", bearer("auditor", TENANT_A))
        .json(&serde_json::json!({ "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("auditor", TENANT_A))
        .json(&serde_json::json!({
            "worker_id": "w", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    let resp = client
        .delete(format!(
            "{}/api/v1/dead-letters/00000000-0000-4000-8000-000000000000",
            server.base_url
        ))
        .header("Authorization", bearer("auditor", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    // Side-effect-free POSTs allowed.
    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/validate",
            server.base_url
        ))
        .header("Authorization", bearer("auditor", TENANT_A))
        .json(&serde_json::json!({ "format": "bpmn", "source": USER_TASK_BPMN }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "auditor may validate");

    server.stop().await;
}

// --- Cross-tenant isolation ---------------------------------------------------

#[tokio::test]
async fn cross_tenant_access_is_isolated() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    let def_id = deploy_probe(&client, &server).await;

    // Tenant A and tenant B each start an instance of the same definition.
    let (status, body) = start_instance(&client, &server, &def_id, "operator", TENANT_A).await;
    assert_eq!(status, 201);
    let instance_a = body["instance_id"].as_str().unwrap().to_string();
    let (status, body) = start_instance(&client, &server, &def_id, "operator", TENANT_B).await;
    assert_eq!(status, 201);
    let instance_b = body["instance_id"].as_str().unwrap().to_string();

    // Listings are scoped by the VERIFIED claim.
    let (status, body) = get(
        &client,
        &server,
        "/api/v1/process-instances?limit=500",
        "auditor",
        TENANT_A,
    )
    .await;
    assert_eq!(status, 200);
    let ids: Vec<&str> = body["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["instance_id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&instance_a.as_str()),
        "tenant A sees its instance"
    );
    assert!(
        !ids.contains(&instance_b.as_str()),
        "tenant A must not see tenant B's instance"
    );

    // Cross-tenant GET: 404 not_found with the SAME body shape an unknown
    // id produces (status, code, and message template — no existence leak).
    let unknown = "00000000-0000-4000-8000-000000000000";
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{unknown}",
            server.base_url
        ))
        .header("Authorization", bearer("auditor", TENANT_B))
        .send()
        .await
        .unwrap();
    let unknown_body = assert_error(resp, 404, "not_found").await;

    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{instance_a}",
            server.base_url
        ))
        .header("Authorization", bearer("auditor", TENANT_B))
        .send()
        .await
        .unwrap();
    let denied_body = assert_error(resp, 404, "not_found").await;
    let redact =
        |body: &serde_json::Value, id: &str| body["error"].as_str().unwrap().replace(id, "<id>");
    assert_eq!(
        redact(&denied_body, &instance_a),
        redact(&unknown_body, unknown),
        "cross-tenant GET must be indistinguishable from an unknown id"
    );

    // Trace / history / steps of another tenant: 404.
    for suffix in ["trace", "history", "steps"] {
        let resp = client
            .get(format!(
                "{}/api/v1/process-instances/{instance_a}/{suffix}",
                server.base_url
            ))
            .header("Authorization", bearer("auditor", TENANT_B))
            .send()
            .await
            .unwrap();
        assert_error(resp, 404, "not_found").await;
    }

    // Cross-tenant user-task complete: 404 (before any state changes).
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{instance_a}:review/complete",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_B))
        .json(&serde_json::json!({ "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 404, "not_found").await;

    // The owning tenant completes its own task.
    let resp = client
        .post(format!(
            "{}/api/v1/tasks/{instance_a}:review/complete",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .json(&serde_json::json!({ "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Cross-tenant replay-session creation: 404.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{instance_b}/replay",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_error(resp, 404, "not_found").await;

    // Own-tenant replay session is usable only by the creating tenant.
    let resp = client
        .post(format!(
            "{}/api/v1/process-instances/{instance_b}/replay",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_B))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let session_b = body["session_id"].as_str().unwrap().to_string();
    let resp = client
        .get(format!(
            "{}/api/v1/replay/{session_b}/snapshot",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_error(resp, 404, "not_found").await;
    let resp = client
        .get(format!(
            "{}/api/v1/replay/{session_b}/snapshot",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_B))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    server.stop().await;
}

#[tokio::test]
async fn worker_fetch_is_tenant_scoped() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Tenant A's operator starts a payment-flow instance (creates a
    // `payment` external task owned by tenant A).
    let (status, body) =
        start_instance(&client, &server, "payment-flow", "operator", TENANT_A).await;
    assert_eq!(status, 201, "operator starts payment-flow: {body}");

    // Tenant B's worker fetches the same topic: NOTHING is delivered.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_B))
        .json(&serde_json::json!({
            "worker_id": "w-b", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tasks: serde_json::Value = resp.json().await.unwrap();
    assert!(
        tasks.as_array().unwrap().is_empty(),
        "tenant B's worker must not fetch tenant A's tasks: {tasks}"
    );

    // Tenant A's worker fetches it and holds the lease.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({
            "worker_id": "w-a", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let tasks: serde_json::Value = resp.json().await.unwrap();
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();

    // Cross-tenant complete: 400 task_not_found — the same body shape an
    // unknown task id produces (status, code, message template).
    let unknown_task = "00000000-0000-4000-8000-000000000000";
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{unknown_task}/complete",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_B))
        .json(&serde_json::json!({ "worker_id": "w-b", "variables": {} }))
        .send()
        .await
        .unwrap();
    let unknown_body = assert_error(resp, 400, "task_not_found").await;

    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{task_id}/complete",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_B))
        .json(&serde_json::json!({ "worker_id": "w-b", "variables": {} }))
        .send()
        .await
        .unwrap();
    let denied_body = assert_error(resp, 400, "task_not_found").await;
    let redact =
        |body: &serde_json::Value, id: &str| body["error"].as_str().unwrap().replace(id, "<id>");
    assert_eq!(
        redact(&denied_body, &task_id),
        redact(&unknown_body, unknown_task),
        "cross-tenant complete must be indistinguishable from an unknown task id"
    );

    // Cross-tenant fail and extend-lock are refused the same way.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{task_id}/fail",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_B))
        .json(&serde_json::json!({ "worker_id": "w-b", "error": "boom" }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 400, "task_not_found").await;

    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{task_id}/extend-lock",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_B))
        .json(&serde_json::json!({ "worker_id": "w-b", "extension_ms": 5000 }))
        .send()
        .await
        .unwrap();
    assert_error(resp, 400, "task_not_found").await;

    // The owning worker still holds the lease and completes.
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{task_id}/complete",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({ "worker_id": "w-a", "variables": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the lease was never stolen by tenant B");

    server.stop().await;
}

#[tokio::test]
async fn dead_letters_are_tenant_scoped_under_jwt() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // Tenant A: start payment-flow, worker fetches + fails PERMANENTLY
    // (straight to the DLQ, entry tagged with the claim tenant).
    let (status, body) =
        start_instance(&client, &server, "payment-flow", "operator", TENANT_A).await;
    assert_eq!(status, 201, "operator starts payment-flow: {body}");
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({
            "worker_id": "w-a", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    let tasks: serde_json::Value = resp.json().await.unwrap();
    let task_id = tasks[0]["task_id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/{task_id}/fail",
            server.base_url
        ))
        .header("Authorization", bearer("worker", TENANT_A))
        .json(&serde_json::json!({
            "worker_id": "w-a", "error": "permanent-ish", "category": "permanent"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Tenant A's operator sees the dead letter; tenant B's does not.
    let (status, body) = get(
        &client,
        &server,
        "/api/v1/dead-letters",
        "operator",
        TENANT_A,
    )
    .await;
    assert_eq!(status, 200);
    let entries_a = body["entries"].as_array().unwrap().len();
    assert_eq!(entries_a, 1, "tenant A sees its dead letter: {body}");
    let entry_id = body["entries"][0]["id"].as_str().unwrap().to_string();

    let (status, body) = get(
        &client,
        &server,
        "/api/v1/dead-letters",
        "operator",
        TENANT_B,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        body["entries"].as_array().unwrap().is_empty(),
        "tenant B must not see tenant A's dead letters: {body}"
    );

    // Tenant B's operator cannot get/requeue/delete it (404, like an
    // unknown id).
    let resp = client
        .get(format!(
            "{}/api/v1/dead-letters/{entry_id}",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_B))
        .send()
        .await
        .unwrap();
    assert_error(resp, 404, "not_found").await;

    let resp = client
        .post(format!(
            "{}/api/v1/dead-letters/{entry_id}/requeue",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_B))
        .send()
        .await
        .unwrap();
    assert_error(resp, 404, "not_found").await;

    let resp = client
        .delete(format!(
            "{}/api/v1/dead-letters/{entry_id}",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_B))
        .send()
        .await
        .unwrap();
    assert_error(resp, 404, "not_found").await;

    // Tenant A's operator requeues it.
    let resp = client
        .post(format!(
            "{}/api/v1/dead-letters/{entry_id}/requeue",
            server.base_url
        ))
        .header("Authorization", bearer("operator", TENANT_A))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "tenant A may requeue its dead letter");

    server.stop().await;
}

// --- X-Tenant-Id deprecation --------------------------------------------------

#[tokio::test]
async fn conflicting_tenant_header_is_forbidden() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // A header DISAGREEING with the claim: 403 forbidden.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("auditor", TENANT_A))
        .header("X-Tenant-Id", TENANT_B)
        .send()
        .await
        .unwrap();
    assert_error(resp, 403, "forbidden").await;

    // A header AGREEING with the claim: accepted (and ignored — the claim
    // scopes the request either way).
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("auditor", TENANT_A))
        .header("X-Tenant-Id", TENANT_A)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // No header: accepted.
    let (status, _) = get(
        &client,
        &server,
        "/api/v1/process-instances",
        "auditor",
        TENANT_A,
    )
    .await;
    assert_eq!(status, 200);

    server.stop().await;
}

// --- Admin API-key bootstrap path ----------------------------------------------

#[tokio::test]
async fn api_key_is_the_full_access_bootstrap_path() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();

    // No bearer token + valid API key: full access, no role checks.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", TEST_API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the admin key reads");

    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            server.base_url
        ))
        .header("X-API-Key", TEST_API_KEY)
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "the admin key deploys (no role checks)");

    let resp = client
        .post(format!(
            "{}/api/v1/external-tasks/fetch-and-lock",
            server.base_url
        ))
        .header("X-API-Key", TEST_API_KEY)
        .json(&serde_json::json!({
            "worker_id": "w-admin", "task_types": ["payment"], "lock_duration_ms": 60000
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "the admin key may even act as a worker (full access)"
    );

    // The API-key path honors X-Tenant-Id (untrusted client input, as
    // before E5-1).
    let resp = client
        .post(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", TEST_API_KEY)
        .header("X-Tenant-Id", "header-tenant")
        .json(&serde_json::json!({ "process_def_id": "minimal" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances?limit=500",
            server.base_url
        ))
        .header("X-API-Key", TEST_API_KEY)
        .header("X-Tenant-Id", "header-tenant")
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["instances"].as_array().unwrap().len(),
        1,
        "the header scopes the admin path's listing"
    );

    // A presented JWT takes precedence over a (wrong) API key.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("Authorization", bearer("auditor", TENANT_A))
        .header("X-API-Key", "wrong-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "JWT wins when both are presented");

    // Without any credential the JWT requirement stands.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "invalid_token").await;

    // An invalid key on the fallback path keeps the legacy code.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", "wrong-key")
        .send()
        .await
        .unwrap();
    assert_error(resp, 401, "unauthorized").await;

    server.stop().await;
}
