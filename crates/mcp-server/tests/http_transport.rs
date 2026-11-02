//! End-to-end tests for the MCP HTTP+SSE transport (N5-1): the REAL axum
//! router (`serve_on`) over an ephemeral loopback listener, driven through
//! a real reqwest streaming SSE client. No engine is spawned: the
//! exercised requests (`initialize`, `tools/list`, `ping`) never dispatch
//! to the REST client, so a `NoopCaller` suffices and the test is fast and
//! self-contained.
//!
//! Covered:
//! - `GET /sse` opens the stream and the FIRST event is `endpoint` naming
//!   the POST URL with a session id;
//! - `POST /messages` with a JSON-RPC request answers `202 Accepted` and
//!   the JSON-RPC reply arrives as a `message` SSE event on the stream;
//! - `tools/list` returns the non-empty default (read-only) tool surface;
//! - a notification (`notifications/initialized`) produces NO SSE event —
//!   the next event belongs to the next REQUEST (id pinning proves the
//!   ordering, mirroring the stdio suite's notification-silence check).

use bpm_engine_mcp_server::allowlist;
use bpm_engine_mcp_server::protocol::{McpServer, ToolCallOutcome, ToolCaller};
use bpm_engine_mcp_server::tools::RestRequest;
use bpm_engine_mcp_server::transport_http::serve_on;
use bpm_engine_mcp_server::BoxCallFuture;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_stream::StreamExt as _;

/// A caller that never dispatches (this suite only exercises initialize,
/// tools/list, and ping — none of which reach the engine).
struct NoopCaller;

impl ToolCaller for NoopCaller {
    fn call_tool<'a>(&'a self, _request: RestRequest, _tool_name: &'a str) -> BoxCallFuture<'a> {
        Box::pin(async {
            ToolCallOutcome {
                is_error: false,
                text: "{}".to_string(),
            }
        })
    }
}

/// Read the next non-empty SSE event (skipping keep-alive comment blocks)
/// from a byte stream: returns `(event, data)`. Panics on a stream error or
/// non-UTF-8 bytes.
async fn next_sse_event<S, T, E>(stream: &mut S) -> Option<(String, String)>
where
    S: tokio_stream::Stream<Item = Result<T, E>> + Unpin,
    T: AsRef<[u8]>,
    E: std::fmt::Debug,
{
    let mut buf = String::new();
    loop {
        if let Some(idx) = buf.find("\n\n") {
            let block: String = buf.drain(..idx + 2).collect();
            let (event, data) = parse_sse_event(&block);
            if event.is_empty() && data.is_empty() {
                continue; // keep-alive comment (`: …`) or empty block
            }
            return Some((event, data));
        }
        match stream.next().await {
            Some(Ok(chunk)) => {
                buf.push_str(std::str::from_utf8(chunk.as_ref()).expect("sse chunk is utf-8"));
            }
            Some(Err(e)) => panic!("sse stream error: {e:?}"),
            None => return None, // the stream closed
        }
    }
}

/// Parse one SSE event block (text between `\n\n` boundaries) into its
/// `event` and `data` fields. `data:` lines are joined with `\n` (one
/// leading space after `data:` is stripped, per the SSE spec); comment
/// lines (`:`) are ignored.
fn parse_sse_event(block: &str) -> (String, String) {
    let mut event = String::new();
    let mut data_parts: Vec<String> = Vec::new();
    for line in block.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            // Strip exactly one leading space (SSE spec); the rest is data.
            let data = rest.strip_prefix(' ').unwrap_or(rest);
            data_parts.push(data.to_string());
        }
        // Lines starting with `:` are comments; everything else is ignored.
    }
    (event, data_parts.join("\n"))
}

#[tokio::test]
async fn http_transport_serves_initialize_tools_list_and_ping_over_sse() {
    // Bound the whole scenario so a regression hangs the test, not CI.
    let result = tokio::time::timeout(Duration::from_secs(15), run_http_scenario()).await;
    assert!(
        result.is_ok(),
        "the HTTP+SSE scenario must finish within the timeout"
    );
}

async fn run_http_scenario() {
    let names = allowlist::resolve(None, None).expect("default allowlist");
    let server = McpServer::new(Box::new(NoopCaller), &names).expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let serve_task = tokio::spawn(serve_on(server, listener));

    let client = reqwest::Client::new();

    // 1. Open the SSE stream; the first event is the `endpoint` naming the
    //    POST URL (with a session id).
    let sse_url = format!("http://{addr}/sse");
    let resp = client.get(&sse_url).send().await.expect("GET /sse");
    assert_eq!(resp.status(), 200, "GET /sse must succeed");
    let mut stream = resp.bytes_stream();
    let (event, data) = next_sse_event(&mut stream)
        .await
        .expect("an endpoint event arrives");
    assert_eq!(event, "endpoint", "the first event must be the endpoint");
    let post_path = data;
    assert!(
        post_path.starts_with("/messages?session_id="),
        "endpoint data must name the POST URL: {post_path}"
    );
    let session_id = post_path
        .split("session_id=")
        .nth(1)
        .expect("session_id in the endpoint URL")
        .to_string();
    let post_url = format!("http://{addr}/messages?session_id={session_id}");

    // 2. POST initialize → 202, and the reply arrives on the SSE stream.
    let post_resp = client
        .post(&post_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" }
        }))
        .send()
        .await
        .expect("POST initialize");
    assert_eq!(
        post_resp.status(),
        202,
        "POST must return 202 Accepted (the reply travels over SSE)"
    );
    let (event, data) = next_sse_event(&mut stream)
        .await
        .expect("an initialize response event arrives");
    assert_eq!(event, "message");
    let reply: Value = serde_json::from_str(&data).expect("initialize reply is JSON");
    assert_eq!(reply["jsonrpc"], "2.0");
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");

    // 3. POST tools/list → the tool list is NON-EMPTY and equals the
    //    default (read-only) surface.
    let post_resp = client
        .post(&post_url)
        .json(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .send()
        .await
        .expect("POST tools/list");
    assert_eq!(post_resp.status(), 202);
    let (event, data) = next_sse_event(&mut stream)
        .await
        .expect("a tools/list response event arrives");
    assert_eq!(event, "message");
    let reply: Value = serde_json::from_str(&data).expect("tools/list reply is JSON");
    assert_eq!(reply["id"], 2);
    let tools = reply["result"]["tools"]
        .as_array()
        .expect("tools/list result.tools is an array");
    assert!(!tools.is_empty(), "the tool list must be non-empty");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, allowlist::DEFAULT_TOOLS.to_vec());
    // Mutating tools are NOT exposed by the default allowlist.
    assert!(!names.contains(&"start_process_instance"));
    assert!(!names.contains(&"deploy_process_definition"));

    // 4. A notification produces NO SSE event: POST it, then POST a request,
    //    and confirm the NEXT SSE event is the request's reply (the
    //    notification stays silent — JSON-RPC's MUST-NOT-reply rule, over
    //    HTTP+SSE just as over stdio).
    let post_resp = client
        .post(&post_url)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .expect("POST notification");
    assert_eq!(post_resp.status(), 202);
    let post_resp = client
        .post(&post_url)
        .json(&json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}))
        .send()
        .await
        .expect("POST ping");
    assert_eq!(post_resp.status(), 202);
    let (event, data) = next_sse_event(&mut stream)
        .await
        .expect("a ping response event arrives (the notification stayed silent)");
    assert_eq!(event, "message");
    let reply: Value = serde_json::from_str(&data).expect("ping reply is JSON");
    assert_eq!(
        reply["id"], 3,
        "the notification produced no event; the next event is ping's reply"
    );
    assert_eq!(reply["result"], json!({}));

    // Tear down: close the SSE connection and stop the server.
    drop(stream);
    serve_task.abort();
}

#[tokio::test]
async fn http_transport_rejects_post_without_a_session() {
    // A POST with no session_id answers 400 (the SSE stream must be opened
    // first so the server knows where to send the response).
    let names = allowlist::resolve(None, None).expect("default allowlist");
    let server = McpServer::new(Box::new(NoopCaller), &names).expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let serve_task = tokio::spawn(serve_on(server, listener));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/messages"))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await
        .expect("POST without session_id");
    assert_eq!(resp.status(), 400, "a missing session_id must be rejected");

    // An unknown session_id answers 404.
    let resp = client
        .post(format!("http://{addr}/messages?session_id=no-such-session"))
        .json(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}))
        .send()
        .await
        .expect("POST with unknown session_id");
    assert_eq!(resp.status(), 404, "an unknown session_id must be rejected");

    serve_task.abort();
}
