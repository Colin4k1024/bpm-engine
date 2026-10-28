//! End-to-end tests for the MCP server (E4-2): the REAL protocol handler
//! dispatching through the REAL HTTP client against a REAL engine REST
//! server — `build_app` in memory mode served on an ephemeral loopback
//! port, with JWT (E5-1) + admin API-key auth configured exactly like
//! `tests/rbac.rs` in the server crate.
//!
//! No external MCP client is needed: the requests enter as stdin LINES
//! (`McpServer::handle_line`) and the engine answers over HTTP, so the
//! whole MCP→JSON-RPC→REST→engine→JSON-RPC→MCP path is exercised
//! in-process.
//!
//! Covered:
//! - the full v1 workflow through MCP: validate (clean + compile errors
//!   as 200 data), a 4xx tool error (invalid format → `isError: true`
//!   carrying the engine error body), list definitions, version listing,
//!   start instance (with an idempotency key replay), get instance,
//!   history, steps, task list, fetch-and-lock, complete, and the final
//!   COMPLETED instance — plus the capabilities tool;
//! - minimal permission: with the DEFAULT allowlist the mutating tools are
//!   neither listed nor callable (-32602);
//! - engine-side RBAC/tenancy surfaced through MCP: a worker-role token
//!   cannot start instances (403 `forbidden`), and cross-tenant reads are
//!   404 `not_found` — both as tool errors carrying the engine body.
//!
//! Memory driver — no Docker needed.

use bpm_engine_mcp_server::allowlist;
use bpm_engine_mcp_server::config::{Credential, McpConfig};
use bpm_engine_mcp_server::http::RestClient;
use bpm_engine_mcp_server::protocol::McpServer;
use bpm_engine_server_rest::bpm_config::{
    AuthConfig, BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use bpm_engine_server_rest::middleware::mint_jwt;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// Fixed signing secret / admin key for the whole test binary (the same
/// pattern as the server crate's `tests/rbac.rs`).
const TEST_SECRET: &str = "mcp-e2e-secret";
const TEST_API_KEY: &str = "mcp-e2e-admin-key";

/// Tenant A (acme) and tenant B (globex) for the tenancy checks.
const TENANT_A: &str = "acme";
const TENANT_B: &str = "globex";

/// A BPMN that compiles cleanly (validate → valid: true).
const CLEAN_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="mcp-clean-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work">
      <extensionElements>
        <taskDefinition type="mcp-work"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
    <sequenceFlow id="f2" sourceRef="work" targetRef="end"/>
  </process>
</definitions>"#;

/// A BPMN that fails to COMPILE (no end event): validate answers HTTP 200
/// with `valid: false` + `compile_errors` — the errors must surface in the
/// tool result content.
const COMPILE_ERROR_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="mcp-broken-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work">
      <extensionElements>
        <taskDefinition type="mcp-broken-work"/>
      </extensionElements>
    </serviceTask>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
  </process>
</definitions>"#;

struct TestServer {
    base_url: String,
    _serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

fn engine_config() -> BpmConfig {
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

async fn spawn_engine() -> TestServer {
    let app = build_app(&engine_config()).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let timer_cancel = app.timer_cancel.clone();
    let router = app.router.clone();
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestServer {
        base_url: format!("http://{addr}"),
        _serve_task: serve_task,
        timer_cancel,
    }
}

impl TestServer {
    async fn stop(self) {
        self.timer_cancel.cancel();
        self._serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Now + one hour.
fn soon() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

/// An MCP server (full v1 surface) authenticating to the engine as the
/// admin API key.
async fn mcp_admin(engine: &TestServer) -> McpServer {
    let config = McpConfig {
        base_url: engine.base_url.clone(),
        credential: Credential::ApiKey(TEST_API_KEY.to_string()),
        rest_timeout: Duration::from_secs(5),
    };
    let names = allowlist::V1_SURFACE
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    McpServer::new(
        Box::new(RestClient::new(&config).expect("rest client")),
        &names,
    )
    .expect("mcp server builds")
}

/// An MCP server (full v1 surface) authenticating as a role-scoped JWT.
async fn mcp_with_role(engine: &TestServer, role: &str, tenant: &str) -> McpServer {
    let token = mint_jwt(TEST_SECRET, "mcp-e2e", tenant, role, soon()).expect("mint token");
    let config = McpConfig {
        base_url: engine.base_url.clone(),
        credential: Credential::Bearer(token),
        rest_timeout: Duration::from_secs(5),
    };
    let names = allowlist::V1_SURFACE
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    McpServer::new(
        Box::new(RestClient::new(&config).expect("rest client")),
        &names,
    )
    .expect("mcp server builds")
}

/// An MCP server with the DEFAULT (read-only) allowlist.
async fn mcp_default(engine: &TestServer) -> McpServer {
    let config = McpConfig {
        base_url: engine.base_url.clone(),
        credential: Credential::ApiKey(TEST_API_KEY.to_string()),
        rest_timeout: Duration::from_secs(5),
    };
    let names = allowlist::resolve(None, None).expect("default allowlist");
    McpServer::new(
        Box::new(RestClient::new(&config).expect("rest client")),
        &names,
    )
    .expect("mcp server builds")
}

/// Send a line, get the parsed JSON-RPC reply.
async fn call(server: &McpServer, line: impl AsRef<str>) -> Value {
    let reply = server
        .handle_line(line.as_ref())
        .await
        .expect("every request gets a response");
    serde_json::from_str(&reply).expect("reply is JSON")
}

fn tools_call(id: u64, name: &str, arguments: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    }))
    .expect("serializes")
}

/// Call a tool and return (is_error, parsed-body-or-text).
async fn tool_call(server: &McpServer, id: u64, name: &str, arguments: Value) -> (bool, Value) {
    let reply = call(server, tools_call(id, name, arguments)).await;
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool {name} produced no content text: {reply}"))
        .to_string();
    let is_error = reply["result"]["isError"]
        .as_bool()
        .unwrap_or_else(|| panic!("tool {name} produced no isError: {reply}"));
    let body = serde_json::from_str(&text).unwrap_or(json!({ "raw": text }));
    (is_error, body)
}

/// Call a tool expecting a 2xx JSON body.
async fn tool_ok(server: &McpServer, id: u64, name: &str, arguments: Value) -> Value {
    let (is_error, body) = tool_call(server, id, name, arguments).await;
    assert!(!is_error, "tool {name} failed: {body}");
    body
}

// ---------------------------------------------------------------------------
// The full v1 workflow through MCP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_workflow_through_mcp() {
    let engine = spawn_engine().await;
    let mcp = mcp_admin(&engine).await;
    let mut id = 0;
    let mut next_id = || {
        id += 1;
        id
    };

    // Handshake first: initialize + initialized notification.
    let reply = call(
        &mcp,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#,
    )
    .await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    // The initialized NOTIFICATION gets no response at all.
    let silent = mcp
        .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await;
    assert!(silent.is_none(), "notifications are silent");

    // validate — clean BPMN: valid, no critical findings.
    let body = tool_ok(
        &mcp,
        next_id(),
        "validate_process_definition",
        json!({ "format": "bpmn", "source": CLEAN_BPMN }),
    )
    .await;
    assert_eq!(body["valid"], json!(true), "validate body: {body}");

    // validate — compile errors: HTTP 200 data, the errors SURFACE in the
    // tool result content (valid: false + compile_errors).
    let (is_error, body) = tool_call(
        &mcp,
        next_id(),
        "validate_process_definition",
        json!({ "format": "bpmn", "source": COMPILE_ERROR_BPMN }),
    )
    .await;
    assert!(!is_error, "compile errors are 200 data: {body}");
    assert_eq!(body["valid"], json!(false), "validate body: {body}");
    let compile_errors = body["compile_errors"].as_array().expect("compile_errors");
    assert!(!compile_errors.is_empty(), "compile errors surface: {body}");

    // validate — REST tool error: an EMPTY SOURCE passes the tool schema
    // (a string) but the engine answers 400 `empty_source`; the tool
    // result carries isError=true and the engine's error body verbatim.
    // (A bad `format` enum value is rejected CLIENT-side with -32602 —
    // see the protocol suite.)
    let (is_error, body) = tool_call(
        &mcp,
        next_id(),
        "validate_process_definition",
        json!({ "format": "bpmn", "source": "" }),
    )
    .await;
    assert!(is_error, "400 must be a tool error: {body}");
    assert_eq!(body["code"], json!("empty_source"), "{body}");
    assert!(body["error"].is_string(), "{body}");

    // list definitions (memory mode bootstraps minimal + payment-flow).
    let body = tool_ok(&mcp, next_id(), "list_process_definitions", json!({})).await;
    let defs = body["definitions"].as_array().expect("definitions array");
    let keys: Vec<&str> = defs
        .iter()
        .filter_map(|d| d["key"].as_str().or(d["process_definition_id"].as_str()))
        .collect();
    assert!(
        keys.iter().any(|k| k.contains("minimal")),
        "bootstrap definitions visible: {body}"
    );

    // definition versions for a key.
    let body = tool_ok(
        &mcp,
        next_id(),
        "list_definition_versions",
        json!({ "key": "minimal" }),
    )
    .await;
    assert!(
        body["versions"].as_array().is_some() || body.to_string().contains("version"),
        "versions body: {body}"
    );

    // start an instance of the bootstrap payment-flow (external task
    // "payment"), with an idempotency key.
    let (is_error, body) = tool_call(
        &mcp,
        next_id(),
        "start_process_instance",
        json!({
            "process_def_id": "payment-flow",
            "variables": { "amount": "42" },
            "idempotency_key": "mcp-e2e-1",
        }),
    )
    .await;
    assert!(!is_error, "start failed: {body}");
    let instance_id = body["instance_id"]
        .as_str()
        .or_else(|| body["process_instance_id"].as_str())
        .expect("instance_id in the start body")
        .to_string();

    // The same idempotency key replays the recorded instance (200).
    let (is_error, replay) = tool_call(
        &mcp,
        next_id(),
        "start_process_instance",
        json!({
            "process_def_id": "payment-flow",
            "idempotency_key": "mcp-e2e-1",
        }),
    )
    .await;
    assert!(!is_error, "idempotent replay failed: {replay}");
    let replayed_id = replay["instance_id"]
        .as_str()
        .or_else(|| replay["process_instance_id"].as_str())
        .expect("replayed instance_id");
    assert_eq!(replayed_id, instance_id, "idempotency key replays");

    // get instance: RUNNING, waiting at the payment external task.
    let body = tool_ok(
        &mcp,
        next_id(),
        "get_process_instance",
        json!({ "id": instance_id }),
    )
    .await;
    assert_eq!(body["status"], json!("RUNNING"), "{body}");
    let current_nodes = body["current_nodes"].as_array().expect("current_nodes");
    assert!(
        current_nodes.iter().any(|n| n == "payment"),
        "token waits at the payment task: {body}"
    );

    // history: the ProcessStarted event is recorded.
    let body = tool_ok(
        &mcp,
        next_id(),
        "get_instance_history",
        json!({ "id": instance_id }),
    )
    .await;
    let history = body.to_string();
    assert!(history.contains("ProcessStarted"), "history: {history}");

    // steps: the step timeline exists.
    let body = tool_ok(
        &mcp,
        next_id(),
        "get_instance_steps",
        json!({ "id": instance_id }),
    )
    .await;
    assert!(body["steps"].is_array(), "steps: {body}");

    // tasks: the external payment task is listed.
    let body = tool_ok(
        &mcp,
        next_id(),
        "list_tasks",
        json!({ "type": "external", "limit": 10 }),
    )
    .await;
    let tasks = body.to_string();
    assert!(tasks.contains("payment"), "task list: {tasks}");

    // fetch-and-lock the payment task as a worker (the endpoint answers a
    // BARE ARRAY of locked tasks).
    let body = tool_ok(
        &mcp,
        next_id(),
        "fetch_and_lock_external_tasks",
        json!({
            "worker_id": "mcp-e2e-worker",
            "task_types": ["payment"],
            "max_tasks": 1,
            "lock_duration_ms": 60000,
        }),
    )
    .await;
    let tasks = body.as_array().expect("locked tasks array");
    let task = tasks
        .iter()
        .find(|t| t["task_id"].is_string())
        .expect("a locked task");
    let task_id = task["task_id"].as_str().expect("task_id").to_string();
    assert_eq!(task["task_type"], json!("payment"));

    // complete the task with result variables.
    let (is_error, body) = tool_call(
        &mcp,
        next_id(),
        "complete_external_task",
        json!({
            "task_id": task_id,
            "worker_id": "mcp-e2e-worker",
            "variables": { "payment_ok": "yes" },
        }),
    )
    .await;
    assert!(!is_error, "complete failed: {body}");

    // the instance is COMPLETED.
    let body = tool_ok(
        &mcp,
        next_id(),
        "get_process_instance",
        json!({ "id": instance_id }),
    )
    .await;
    assert_eq!(body["status"], json!("COMPLETED"), "{body}");
    let variables = body["variables"].as_object().expect("variables");
    assert!(variables.contains_key("payment_ok"), "{body}");

    // capabilities: the catalog itself is reachable as a tool.
    let body = tool_ok(&mcp, next_id(), "get_capabilities", json!({})).await;
    assert_eq!(body["catalog_version"], json!("1"), "{body}");
    // 41 = 35 (E4-1) + 5 (E5-2 approvals/lifecycle) + 1 (E5-3 audit view).
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(41));

    engine.stop().await;
}

// ---------------------------------------------------------------------------
// Minimal permission (allowlist) — mutating tools are opt-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_allowlist_hides_mutating_tools() {
    let engine = spawn_engine().await;
    let mcp = mcp_default(&engine).await;

    // tools/list: only the read-only surface.
    let reply = call(&mcp, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await;
    let tools = reply["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, allowlist::DEFAULT_TOOLS.to_vec());
    assert!(!names.contains(&"start_process_instance"));
    assert!(!names.contains(&"deploy_process_definition"));

    // Calling a hidden mutating tool: JSON-RPC -32602 unknown tool.
    let reply = call(
        &mcp,
        tools_call(
            2,
            "start_process_instance",
            json!({ "process_def_id": "minimal" }),
        ),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool"),
        "{reply}"
    );

    // And the read-only tools still work end to end.
    let (is_error, body) = tool_call(
        &mcp,
        3,
        "list_process_instances",
        json!({ "limit": 1, "offset": 0 }),
    )
    .await;
    assert!(!is_error, "read tool failed: {body}");
    assert!(body["instances"].as_array().is_some(), "{body}");

    engine.stop().await;
}

// ---------------------------------------------------------------------------
// Engine-side RBAC + tenancy, surfaced through MCP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_rbac_and_tenancy_surface_through_mcp() {
    let engine = spawn_engine().await;

    // An operator (tenant acme) starts an instance through MCP.
    let operator = mcp_with_role(&engine, "operator", TENANT_A).await;
    let (is_error, body) = tool_call(
        &operator,
        1,
        "start_process_instance",
        json!({ "process_def_id": "minimal", "idempotency_key": "rbac-1" }),
    )
    .await;
    assert!(!is_error, "operator start failed: {body}");
    let instance_id = body["instance_id"]
        .as_str()
        .or_else(|| body["process_instance_id"].as_str())
        .expect("instance_id")
        .to_string();

    // A WORKER token (role check, engine-side): starting instances is an
    // operator permission — the engine answers 403, and the MCP tool
    // result carries the error body verbatim with isError=true.
    let worker = mcp_with_role(&engine, "worker", TENANT_A).await;
    let (is_error, body) = tool_call(
        &worker,
        2,
        "start_process_instance",
        json!({ "process_def_id": "minimal" }),
    )
    .await;
    assert!(is_error, "worker start must fail: {body}");
    assert_eq!(body["code"], json!("forbidden"), "{body}");

    // TENANT validation (engine-side, from the verified claim): the
    // worker of tenant B cannot read tenant A's instance — 404
    // not_found, indistinguishable from an unknown id.
    let worker_b = mcp_with_role(&engine, "worker", TENANT_B).await;
    let (is_error, body) = tool_call(
        &worker_b,
        3,
        "get_process_instance",
        json!({ "id": instance_id }),
    )
    .await;
    assert!(is_error, "cross-tenant read must fail: {body}");
    assert_eq!(body["code"], json!("not_found"), "{body}");

    // The tenant-scoped listing of tenant B does NOT contain A's instance.
    let (is_error, body) = tool_call(&worker_b, 4, "list_process_instances", json!({})).await;
    assert!(!is_error, "listing failed: {body}");
    let listing = body.to_string();
    assert!(
        !listing.contains(&instance_id),
        "tenant B sees A's instance: {listing}"
    );

    // An auth-free tool still works for a role-scoped token.
    let (is_error, body) = tool_call(&worker_b, 5, "get_capabilities", json!({})).await;
    assert!(!is_error, "capabilities failed: {body}");
    assert_eq!(body["catalog_version"], json!("1"));

    // A garbage token surfaces as 401 invalid_token through the tool.
    let bad_config = McpConfig {
        base_url: engine.base_url.clone(),
        credential: Credential::Bearer("not-a-jwt".to_string()),
        rest_timeout: Duration::from_secs(5),
    };
    let bad = McpServer::new(
        Box::new(RestClient::new(&bad_config).expect("rest client")),
        &allowlist::resolve(None, Some("get_process_instance")).expect("allowlist"),
    )
    .expect("mcp server builds");
    let (is_error, body) = tool_call(
        &bad,
        6,
        "get_process_instance",
        json!({ "id": "00000000-0000-4000-8000-000000000000" }),
    )
    .await;
    assert!(is_error, "bad token must fail: {body}");
    assert_eq!(body["code"], json!("invalid_token"), "{body}");

    engine.stop().await;
}

// ---------------------------------------------------------------------------
// The 10 s REST timeout, as a tool error (fast: refused TCP connect)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unreachable_engine_is_a_tool_error_not_a_hang() {
    // Port 1 on loopback: connection-refused immediately in CI containers
    // and dev machines alike (nothing listens there).
    let config = McpConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        credential: Credential::None,
        rest_timeout: Duration::from_secs(10),
    };
    let mcp = McpServer::new(
        Box::new(RestClient::new(&config).expect("rest client")),
        &allowlist::resolve(None, Some("list_process_definitions")).expect("allowlist"),
    )
    .expect("mcp server builds");
    let (is_error, body) = tool_call(&mcp, 1, "list_process_definitions", json!({})).await;
    assert!(is_error, "unreachable engine must be a tool error: {body}");
    assert!(
        body["raw"].as_str().unwrap_or_default().contains("failed"),
        "transport failure text: {body}"
    );
}
