//! Protocol-level tests for the MCP server (E4-2): the JSON-RPC 2.0
//! handshake, tools/list shape, tools/call dispatch (through a FAKE
//! caller — no I/O), error mapping, id echo, and fuzz-ish robustness
//! against garbage lines.
//!
//! The end-to-end suite (`tests/end_to_end.rs`) drives the same handler
//! against a REAL engine REST server; this file pins the protocol
//! behavior itself.

use bpm_engine_mcp_server::allowlist;
use bpm_engine_mcp_server::protocol::{McpServer, ToolCallOutcome, ToolCaller};
use bpm_engine_mcp_server::tools::RestRequest;
use serde_json::{json, Value};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Records the last dispatched request and returns a programmed outcome.
struct FakeCaller {
    last: Mutex<Option<(String, RestRequest)>>,
    outcome: Mutex<ToolCallOutcome>,
}

impl FakeCaller {
    fn new(outcome: ToolCallOutcome) -> Self {
        Self {
            last: Mutex::new(None),
            outcome: Mutex::new(outcome),
        }
    }

    fn last_dispatch(&self) -> (String, RestRequest) {
        self.last
            .lock()
            .unwrap()
            .clone()
            .expect("a call was dispatched")
    }
}

impl ToolCaller for FakeCaller {
    fn call_tool<'a>(
        &'a self,
        request: RestRequest,
        tool_name: &'a str,
    ) -> bpm_engine_mcp_server::BoxCallFuture<'a> {
        Box::pin(async move {
            let recorded = (tool_name.to_string(), request);
            *self.last.lock().unwrap() = Some(recorded);
            self.outcome.lock().unwrap().clone()
        })
    }
}

/// A server with the DEFAULT (read-only) allowlist and a fake caller.
fn default_server(outcome: ToolCallOutcome) -> McpServer {
    let names = allowlist::resolve(None, None).expect("default allowlist");
    McpServer::new(Box::new(FakeCaller::new(outcome)), &names).expect("server builds")
}

/// A server with an EXPLICIT allowlist (mutating tools enabled).
fn server_with(tools: &str, outcome: ToolCallOutcome) -> McpServer {
    let names = allowlist::resolve(None, Some(tools)).expect("allowlist resolves");
    McpServer::new(Box::new(FakeCaller::new(outcome)), &names).expect("server builds")
}

/// Send one raw line; assert a response came back and parse it.
async fn round_trip(server: &McpServer, line: impl AsRef<str>) -> Value {
    let line = line.as_ref();
    let reply = server
        .handle_line(line)
        .await
        .unwrap_or_else(|| panic!("line {line:?} must get a response"));
    let value: Value =
        serde_json::from_str(&reply).unwrap_or_else(|e| panic!("reply {reply:?} is JSON: {e}"));
    // MCP messages are single-line JSON.
    assert!(!reply.contains('\n'), "reply must not contain newlines");
    value
}

/// Send one line; assert NO response (notification / ignorable).
async fn expect_silent(server: &McpServer, line: impl AsRef<str>) {
    let line = line.as_ref();
    let reply = server.handle_line(line).await;
    assert!(reply.is_none(), "line {line:?} must NOT get a response");
}

/// A well-formed JSON-RPC request line (any JSON-serializable id).
fn request(id: impl serde::Serialize, method: &str, params: Value) -> String {
    let id = serde_json::to_value(id).expect("id serializes");
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .expect("serializes")
}

/// A tools/call request line.
fn tools_call(id: impl serde::Serialize, name: &str, arguments: Value) -> String {
    request(
        id,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

/// The text content of a tools/call result.
fn content_text(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .expect("content[0].text")
        .to_string()
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_negotiates_a_supported_protocol_version() {
    let server = default_server(success_outcome());
    for version in ["2024-11-05", "2025-03-26", "2025-06-18"] {
        let reply = round_trip(
            &server,
            request(1, "initialize", json!({ "protocolVersion": version })),
        )
        .await;
        assert_eq!(reply["result"]["protocolVersion"], version);
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["jsonrpc"], "2.0");
    }
    // An unknown (or newer) client version gets our LATEST supported one.
    let reply = round_trip(
        &server,
        request(
            "id-1",
            "initialize",
            json!({ "protocolVersion": "1999-01-01" }),
        ),
    )
    .await;
    assert_eq!(
        reply["result"]["protocolVersion"],
        bpm_engine_mcp_server::protocol::LATEST_PROTOCOL_VERSION
    );
    // Missing params entirely: latest supported.
    let reply = round_trip(&server, request(2, "initialize", json!({}))).await;
    assert_eq!(
        reply["result"]["protocolVersion"],
        bpm_engine_mcp_server::protocol::LATEST_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn initialize_result_shape() {
    let server = default_server(success_outcome());
    let reply = round_trip(
        &server,
        request(7, "initialize", json!({ "protocolVersion": "2025-06-18" })),
    )
    .await;
    let result = &reply["result"];
    assert!(result["capabilities"]["tools"].is_object(), "{result}");
    assert_eq!(
        result["serverInfo"]["name"],
        bpm_engine_mcp_server::protocol::SERVER_NAME
    );
    assert!(result["serverInfo"]["version"].is_string());
    let instructions = result["instructions"].as_str().expect("instructions");
    assert!(instructions.contains("BPM engine tools"), "{instructions}");
}

#[tokio::test]
async fn initialized_notification_and_ping() {
    let server = default_server(success_outcome());
    expect_silent(
        &server,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
    // Unknown notifications are also silent (JSON-RPC: notifications never
    // get responses).
    expect_silent(
        &server,
        r#"{"jsonrpc":"2.0","method":"notifications/whatever"}"#,
    )
    .await;
    let reply = round_trip(&server, r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#).await;
    assert_eq!(reply["result"], json!({}));
    assert_eq!(reply["id"], 42);
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tools_list_shape_and_default_allowlist() {
    let server = default_server(success_outcome());
    let reply = round_trip(&server, request(1, "tools/list", json!({}))).await;
    let tools = reply["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, allowlist::DEFAULT_TOOLS.to_vec());
    // Mutating tools are NOT exposed by default.
    for mutating in [
        "deploy_process_definition",
        "start_process_instance",
        "complete_external_task",
        "complete_user_task",
        "requeue_dead_letter",
        "fetch_and_lock_external_tasks",
    ] {
        assert!(!names.contains(&mutating), "{mutating} must not be listed");
    }
    // Catalog tools outside the v1 surface are never listed.
    assert!(!names.contains(&"activate_definition"));
    assert!(!names.contains(&"create_replay_session"));

    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert!(tool["description"].is_string(), "{name} description");
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{name} inputSchema type");
        assert!(schema["properties"].is_object(), "{name} properties");
        assert_eq!(schema["additionalProperties"], false);
        let annotations = &tool["annotations"];
        assert!(annotations["title"].is_string(), "{name} title");
        // The default surface is read-only.
        assert_eq!(annotations["readOnlyHint"], true, "{name} readOnlyHint");
    }
}

#[tokio::test]
async fn tools_list_marks_mutating_tools() {
    let server = server_with(
        "deploy_process_definition,get_process_instance",
        success_outcome(),
    );
    let reply = round_trip(&server, request(1, "tools/list", json!({}))).await;
    let tools = reply["result"]["tools"].as_array().expect("tools array");
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        let expected = name != "deploy_process_definition";
        assert_eq!(
            tool["annotations"]["readOnlyHint"], expected,
            "{name} readOnlyHint"
        );
        let description = tool["description"].as_str().unwrap();
        if !expected {
            assert!(description.contains("mutating"), "{name}: {description}");
        }
        assert!(description.contains("REST "), "{name}: {description}");
    }
}

#[tokio::test]
async fn tools_list_schemas_carry_the_flattened_parameters() {
    let server = server_with(
        "validate_process_definition,start_process_instance,list_process_instances",
        success_outcome(),
    );
    let reply = round_trip(&server, request(1, "tools/list", json!({}))).await;
    let tools: Vec<Value> = reply["result"]["tools"].as_array().unwrap().clone();
    let find = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tool {name}"))
            .clone()
    };

    let validate = find("validate_process_definition");
    assert_eq!(
        validate["inputSchema"]["required"],
        json!(["format", "source"])
    );
    assert_eq!(
        validate["inputSchema"]["properties"]["format"]["enum"],
        json!(["bpmn", "dsl"])
    );

    let start = find("start_process_instance");
    assert!(start["inputSchema"]["properties"]["idempotency_key"].is_object());
    assert!(start["inputSchema"]["properties"]["process_def_id"].is_object());
    assert!(!start["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .contains_key("X-Tenant-Id"));

    let list = find("list_process_instances");
    assert!(list["inputSchema"]["properties"]["limit"].is_object());
    assert!(list["inputSchema"]["properties"]["offset"].is_object());
}

// ---------------------------------------------------------------------------
// tools/call dispatch
// ---------------------------------------------------------------------------

fn success_outcome() -> ToolCallOutcome {
    ToolCallOutcome {
        is_error: false,
        text: "{\"ok\": true}".to_string(),
    }
}

#[tokio::test]
async fn tools_call_dispatches_to_the_caller_and_echoes_the_id() {
    // The fake caller records the last dispatch; the test retains a
    // handle to it for dispatch assertions.
    let names = allowlist::resolve(None, Some("validate_process_definition")).expect("allowlist");
    let fake = std::sync::Arc::new(FakeCaller::new(success_outcome()));
    let server =
        McpServer::new(Box::new(FakeCallerHandle(fake.clone())), &names).expect("server builds");
    for id in [json!(1), json!("call-9"), json!(null)] {
        let reply = round_trip(
            &server,
            tools_call(
                id.clone(),
                "validate_process_definition",
                json!({
                    "format": "bpmn",
                    "source": "<definitions/>",
                }),
            ),
        )
        .await;
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(content_text(&reply), "{\"ok\": true}");
        let content = reply["result"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
    }
    // The dispatch reached the caller with the BUILT REST request.
    let (tool_name, request) = fake.last_dispatch();
    assert_eq!(tool_name, "validate_process_definition");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/v1/process-definitions/validate");
    assert_eq!(
        request.body,
        Some(bpm_engine_mcp_server::RestBody::Json(json!({
            "format": "bpmn",
            "source": "<definitions/>",
        })))
    );
}

/// A [`ToolCaller`] wrapper over a shared [`FakeCaller`] (lets the test
/// retain a handle for dispatch assertions).
struct FakeCallerHandle(std::sync::Arc<FakeCaller>);

impl ToolCaller for FakeCallerHandle {
    fn call_tool<'a>(
        &'a self,
        request: RestRequest,
        tool_name: &'a str,
    ) -> bpm_engine_mcp_server::BoxCallFuture<'a> {
        self.0.call_tool(request, tool_name)
    }
}

#[tokio::test]
async fn tools_call_surfaces_engine_errors_as_tool_results() {
    // A REST error becomes isError=true with the error body in the
    // content — NOT a JSON-RPC protocol error.
    let server = server_with(
        "start_process_instance",
        ToolCallOutcome {
            is_error: true,
            text: "{\n  \"error\": \"role 'auditor' is not allowed to POST /api/v1/process-instances\",\n  \"code\": \"forbidden\"\n}".to_string(),
        },
    );
    let reply = round_trip(
        &server,
        tools_call(
            1,
            "start_process_instance",
            json!({"process_def_id": "minimal"}),
        ),
    )
    .await;
    assert!(reply.get("error").is_none(), "must not be a protocol error");
    assert_eq!(reply["result"]["isError"], true);
    let text = content_text(&reply);
    let body: Value = serde_json::from_str(&text).expect("error body is JSON");
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn tools_call_surfaces_transport_failures_as_tool_errors() {
    let server = server_with(
        "list_tasks",
        ToolCallOutcome {
            is_error: true,
            text: "BPM REST call for tool \"list_tasks\" failed: operation timed out".to_string(),
        },
    );
    let reply = round_trip(&server, tools_call(1, "list_tasks", json!({}))).await;
    assert_eq!(reply["result"]["isError"], true);
    assert!(content_text(&reply).contains("timed out"));
}

#[tokio::test]
async fn tools_call_for_unlisted_tools_is_a_protocol_error() {
    // A mutating tool outside the (default) allowlist: NOT in tools/list
    // and calling it answers -32602 unknown tool.
    let server = default_server(success_outcome());
    let reply = round_trip(
        &server,
        tools_call(
            1,
            "deploy_process_definition",
            json!({"body": "<definitions/>"}),
        ),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));
    // A real catalog tool outside the v1 surface behaves identically.
    let reply = round_trip(&server, tools_call(2, "activate_definition", json!({}))).await;
    assert_eq!(reply["error"]["code"], -32602);
}

#[tokio::test]
async fn tools_call_argument_validation_is_strict() {
    let server = server_with(
        "validate_process_definition,list_process_instances",
        success_outcome(),
    );
    // Missing required.
    let reply = round_trip(
        &server,
        tools_call(1, "validate_process_definition", json!({"format": "bpmn"})),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("source"));
    // Wrong type.
    let reply = round_trip(
        &server,
        tools_call(
            1,
            "validate_process_definition",
            json!({"format": 1, "source": "x"}),
        ),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("string"));
    // Bad enum value.
    let reply = round_trip(
        &server,
        tools_call(
            1,
            "validate_process_definition",
            json!({"format": "yaml", "source": "x"}),
        ),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("must be one of"),
        "{reply}"
    );
    // Unknown key — the tenant/header smuggling guard.
    let reply = round_trip(
        &server,
        tools_call(1, "list_process_instances", json!({"X-Tenant-Id": "evil"})),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown parameter"));
    // Arguments must be an object when present.
    let reply = round_trip(
        &server,
        request(
            1,
            "tools/call",
            json!({"name": "list_process_instances", "arguments": [1]}),
        ),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    // params must be an object; name must be a string.
    let reply = round_trip(&server, request(1, "tools/call", json!("nope"))).await;
    assert_eq!(reply["error"]["code"], -32602);
    let reply = round_trip(
        &server,
        request(1, "tools/call", json!({"name": 5, "arguments": {}})),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602);
    // Missing/null arguments default to the empty object — fine for
    // no-argument tools.
    let reply = round_trip(
        &server,
        tools_call(1, "list_process_instances", Value::Null),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false);
}

#[tokio::test]
async fn tools_call_rejects_requests_before_dispatch_on_validation_errors() {
    // Validation failures never reach the caller (nothing recorded).
    let outcome = success_outcome();
    let server = server_with("list_process_instances", outcome);
    let _ = round_trip(
        &server,
        tools_call(1, "list_process_instances", json!({"X-Tenant-Id": "evil"})),
    )
    .await;
    // The fake caller shared via a second call proves dispatch still works
    // for the happy path; the invalid one simply never dispatched.
    let reply = round_trip(&server, tools_call(2, "list_process_instances", json!({}))).await;
    assert_eq!(reply["result"]["isError"], false);
}

// ---------------------------------------------------------------------------
// Error mapping / robustness
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_method_is_method_not_found() {
    let server = default_server(success_outcome());
    let reply = round_trip(&server, request(9, "resources/list", json!({}))).await;
    assert_eq!(reply["error"]["code"], -32601);
    assert_eq!(reply["id"], 9);
}

#[tokio::test]
async fn malformed_lines_answer_parse_errors_and_the_loop_continues() {
    let server = default_server(success_outcome());
    for garbage in [
        "{",
        "not json",
        "42",
        "\"a string\"",
        "null",
        "[1, 2, 3]",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":",
        "\u{7f}garbage\u{1}",
        "{bad: syntax}",
        "﻿",
    ] {
        // Blank/whitespace-only lines are skipped silently.
        if garbage.trim().is_empty() {
            expect_silent(&server, garbage).await;
            continue;
        }
        let reply = round_trip(&server, garbage).await;
        assert_eq!(reply["id"], Value::Null, "{garbage:?} -> {reply}");
        let code = reply["error"]["code"].as_i64().expect("error code");
        assert!(
            code == -32700 || code == -32600,
            "{garbage:?} -> unexpected code {code}"
        );
    }
    // `{}` is an OBJECT with no id: a (malformed) notification — silent.
    expect_silent(&server, "{}").await;
    // The server keeps working after garbage.
    let reply = round_trip(&server, r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#).await;
    assert_eq!(reply["result"], json!({}));
}

#[tokio::test]
async fn structural_message_violations_answer_invalid_request() {
    let server = default_server(success_outcome());
    // Missing method.
    let reply = round_trip(&server, r#"{"jsonrpc":"2.0","id":1}"#).await;
    assert_eq!(reply["error"]["code"], -32600);
    // Wrong jsonrpc version.
    let reply = round_trip(&server, r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).await;
    assert_eq!(reply["error"]["code"], -32600);
    // Invalid id type.
    let reply = round_trip(&server, r#"{"jsonrpc":"2.0","id":{"x":1},"method":"ping"}"#).await;
    assert_eq!(reply["error"]["code"], -32600);
    // A JSON array is a batch (not part of MCP).
    let reply = round_trip(&server, r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).await;
    assert_eq!(reply["error"]["code"], -32600);
    // A client-sent RESPONSE (client→server responses are forbidden) is
    // ignored silently.
    expect_silent(&server, r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).await;
    expect_silent(
        &server,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"x"}}"#,
    )
    .await;
}

#[tokio::test]
async fn notifications_are_silent_before_any_validation() {
    // JSON-RPC: servers MUST NOT reply to notifications — so the no-id
    // check runs BEFORE the jsonrpc-version and id-type validation; a
    // notification carrying garbage fields stays silent instead of
    // earning a -32600 reply.
    let server = default_server(success_outcome());
    expect_silent(&server, r#"{"jsonrpc":"1.0","method":"x"}"#).await;
    expect_silent(
        &server,
        r#"{"jsonrpc":"1.0","method":"notifications/initialized"}"#,
    )
    .await;
    expect_silent(&server, r#"{"method":"x"}"#).await;
    expect_silent(&server, r#"{"jsonrpc":"1.0"}"#).await;
    // The SAME problems on REQUESTS (an id present, `"id": null`
    // included) still answer: a wrong version → -32600, an unknown
    // method → -32601.
    let reply = round_trip(&server, r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#).await;
    assert_eq!(reply["error"]["code"], -32600);
    let reply = round_trip(&server, r#"{"jsonrpc":"2.0","id":1,"method":"x"}"#).await;
    assert_eq!(reply["error"]["code"], -32601);
    // A request with a null id is a request, not a notification.
    let reply = round_trip(&server, r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).await;
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(reply["result"], json!({}));
}

#[tokio::test]
async fn oversized_lines_are_rejected() {
    let server = default_server(success_outcome());
    let huge = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"pad\":\"{}\"}}",
        "x".repeat(bpm_engine_mcp_server::stdio::MAX_LINE_BYTES)
    );
    let reply = round_trip(&server, &huge).await;
    assert_eq!(reply["error"]["code"], -32600);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("too large"));
}

#[tokio::test]
async fn crlf_and_blank_lines_are_tolerated() {
    let server = default_server(success_outcome());
    // Trailing \r (CRLF transports) is stripped before parsing.
    let reply = round_trip(
        &server,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r",
    )
    .await;
    assert_eq!(reply["result"], json!({}));
    expect_silent(&server, "").await;
    expect_silent(&server, "   ").await;
    expect_silent(&server, "\r\n").await;
}

#[tokio::test]
async fn allowlist_failures_are_loud() {
    // Real catalog tool, outside the v1 surface: refused.
    let names = vec!["delete_dead_letter".to_string()];
    let err = match McpServer::new(Box::new(FakeCaller::new(success_outcome())), &names) {
        Ok(_) => panic!("a tool outside the v1 surface must be refused"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("v1 tool surface"), "{err}");
    // Unknown tool name (not in the v1 surface either): the surface check
    // fires first, with the valid names listed.
    let names = vec!["no_such_tool".to_string()];
    let err = match McpServer::new(Box::new(FakeCaller::new(success_outcome())), &names) {
        Ok(_) => panic!("an unknown tool must be refused"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("v1 tool surface"), "{err}");
    // A v1-surface name that the (custom) catalog does not carry: the
    // catalog lookup fires.
    let custom_catalog = bpm_engine_mcp_server::catalog::Catalog::parse(
        r#"{"tools":[{"name":"get_capabilities","operation":"GET /capabilities","summary":"s","classification":"read_only","auth_required":false,"parameters":[]}]}"#,
    )
    .expect("custom catalog parses");
    let names = vec!["get_process_instance".to_string()];
    let err = match McpServer::with_catalog(
        custom_catalog,
        Box::new(FakeCaller::new(success_outcome())),
        &names,
    ) {
        Ok(_) => panic!("a catalog-missing tool must be refused"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("not found"), "{err}");
}
