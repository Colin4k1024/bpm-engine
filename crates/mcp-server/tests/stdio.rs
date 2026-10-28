//! Read-loop tests for the stdio transport (E4-2): the REAL `run_over`
//! loop — the same code `run_stdio` drives over the process streams —
//! exercised in-process over tokio duplex pipes.
//!
//! What this pins that `tests/protocol.rs` (handler-level) cannot:
//!
//! - **bounded reading** (the 8 MiB line limit enforced at the READ
//!   layer): an undelimited stream past the limit answers exactly ONE
//!   `-32700` oversized-frame error and the loop STOPS cleanly (a client
//!   streaming bytes without newlines cannot grow memory without
//!   bound); a line arriving WITH its newline but over the limit is
//!   assembled inside the same memory bound and rejected at the message
//!   layer (`-32600`, defense in depth) with the loop CONTINUING;
//! - malformed JSON and invalid-UTF-8 frames answer `-32700` and the
//!   loop keeps reading;
//! - stdin EOF exits cleanly, and a final line WITHOUT its newline is
//!   still answered before the exit (`BufRead::lines` semantics);
//! - notifications stay silent in stream order.
//!
//! The subprocess suite (`tests/stdio_process.rs`) drives the actual
//! binary over real pipes for the same behaviors.

use bpm_engine_mcp_server::allowlist;
use bpm_engine_mcp_server::protocol::{McpServer, ToolCallOutcome, ToolCaller};
use bpm_engine_mcp_server::stdio::{run_over, MAX_LINE_BYTES};
use bpm_engine_mcp_server::tools::RestRequest;
use bpm_engine_mcp_server::BoxCallFuture;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

/// Duplex capacity: one reader quantum (the loop's `READ_CHUNK` is
/// 64 KiB — the frame cap tolerates exactly this much past the message
/// bound before declaring a frame oversized).
const PIPE: usize = 64 * 1024;
/// Per-operation bound (everything here is in-memory; this only guards
/// against a hang).
const TIMEOUT: Duration = Duration::from_secs(10);

/// The client's stdout end (the server's replies, line by line).
type ClientOut = Lines<BufReader<DuplexStream>>;

/// A caller that never dispatches (these tests only use initialize/ping,
/// which do not reach the engine).
struct NoopCaller;

impl ToolCaller for NoopCaller {
    fn call_tool<'a>(&'a self, _request: RestRequest, _tool_name: &'a str) -> BoxCallFuture<'a> {
        Box::pin(async {
            ToolCallOutcome {
                is_error: false,
                text: "{\"ok\":true}".to_string(),
            }
        })
    }
}

/// A server with the DEFAULT (read-only) allowlist.
fn server() -> McpServer {
    let names = allowlist::resolve(None, None).expect("default allowlist");
    McpServer::new(Box::new(NoopCaller), &names).expect("server builds")
}

/// Write one newline-terminated line to the server's stdin.
async fn write_line(stdin: &mut DuplexStream, line: &str) {
    stdin.write_all(line.as_bytes()).await.expect("write line");
    stdin.write_all(b"\n").await.expect("write newline");
    stdin.flush().await.expect("flush");
}

/// Read the next reply line, parsed (with a timeout).
async fn next_reply(stdout: &mut ClientOut) -> Value {
    let line = tokio::time::timeout(TIMEOUT, stdout.next_line())
        .await
        .expect("a reply within the timeout")
        .expect("stdout stays open")
        .expect("a reply line, not EOF");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply {line:?} is JSON: {e}"))
}

/// Assert the server's stdout reached EOF (no further output).
async fn expect_eof(stdout: &mut ClientOut) {
    let line = tokio::time::timeout(TIMEOUT, stdout.next_line())
        .await
        .expect("EOF within the timeout")
        .expect("stdout stays open");
    assert!(
        line.is_none(),
        "expected EOF after the last reply, got {line:?}"
    );
}

// ---------------------------------------------------------------------------
// Session order, malformed lines, and the clean EOF exit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_answers_in_order_and_exits_cleanly_on_eof() {
    let (mut client_in, server_in) = tokio::io::duplex(PIPE);
    let (server_out, client_end) = tokio::io::duplex(PIPE);
    let mut client_out: ClientOut = BufReader::new(client_end).lines();
    let task = tokio::spawn(run_over(server_in, server_out, server()));

    // Handshake.
    write_line(
        &mut client_in,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    )
    .await;
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");

    // A notification gets NO reply: the next reply belongs to the next
    // REQUEST — the id pinning proves the ordering.
    write_line(
        &mut client_in,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
    write_line(
        &mut client_in,
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
    )
    .await;
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["id"], 2, "the notification must stay silent: {reply}");
    assert_eq!(reply["result"], json!({}));

    // A malformed line answers -32700 and the loop KEEPS reading.
    write_line(&mut client_in, "{oops").await;
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["error"]["code"], -32700);
    write_line(
        &mut client_in,
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
    )
    .await;
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["result"], json!({}));

    // A final line WITHOUT its newline, then stdin closed: the partial
    // line is still a message (`BufRead::lines` semantics), answered
    // before the clean exit.
    client_in
        .write_all(br#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#)
        .await
        .expect("write partial line");
    client_in.flush().await.expect("flush");
    drop(client_in); // stdin closed: the MCP shutdown signal.
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["id"], 4);
    assert_eq!(reply["result"], json!({}));

    // EOF: exactly one clean exit, no further output.
    let result = tokio::time::timeout(TIMEOUT, task)
        .await
        .expect("the loop finishes after EOF")
        .expect("the task joins");
    assert!(result.is_ok(), "clean EOF exit, got {result:?}");
    expect_eof(&mut client_out).await;
}

// ---------------------------------------------------------------------------
// Invalid UTF-8: a parse error, and the loop keeps reading
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_utf8_frames_answer_parse_errors_and_keep_the_loop_alive() {
    let (mut client_in, server_in) = tokio::io::duplex(PIPE);
    let (server_out, client_end) = tokio::io::duplex(PIPE);
    let mut client_out: ClientOut = BufReader::new(client_end).lines();
    let task = tokio::spawn(run_over(server_in, server_out, server()));

    // A well-framed line whose bytes are not valid UTF-8: -32700 (the
    // framing is intact — the delimiter was consumed — so this is a
    // malformed MESSAGE, not a broken transport).
    client_in
        .write_all(&[b'{', 0xff, 0xfe, b'}', b'\n'])
        .await
        .expect("write non-UTF-8 frame");
    client_in.flush().await.expect("flush");
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["error"]["code"], -32700, "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("UTF-8"),
        "{reply}"
    );

    // The loop kept reading: the next line works.
    write_line(
        &mut client_in,
        r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#,
    )
    .await;
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["id"], 9);
    assert_eq!(reply["result"], json!({}));

    drop(client_in);
    let result = tokio::time::timeout(TIMEOUT, task)
        .await
        .expect("the loop finishes")
        .expect("the task joins");
    assert!(result.is_ok(), "{result:?}");
}

// ---------------------------------------------------------------------------
// Bounded reading: the undelimited overrun (Finding 1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn undelimited_overrun_answers_one_error_and_stops_cleanly() {
    let (client_in, server_in) = tokio::io::duplex(PIPE);
    let (server_out, client_end) = tokio::io::duplex(PIPE);
    let mut client_out: ClientOut = BufReader::new(client_end).lines();
    let task = tokio::spawn(run_over(server_in, server_out, server()));

    // Stream ~9 MiB with NO newline — past the 8 MiB message bound: the
    // loop must stop reading once the frame is oversized (its reader end
    // is dropped, so this writer sees a closed pipe and stops too).
    let writer = tokio::spawn(async move {
        let mut client_in = client_in;
        let chunk = [b'a'; 64 * 1024];
        let mut remaining = 9 * 1024 * 1024;
        while remaining > 0 {
            let want = chunk.len().min(remaining);
            match client_in.write(&chunk[..want]).await {
                Ok(0) | Err(_) => break, // the server hung up: frame oversized
                Ok(n) => remaining -= n,
            }
        }
    });

    // The loop returns Ok — ONE error answer, then a clean stop.
    let result = tokio::time::timeout(TIMEOUT, task)
        .await
        .expect("the loop terminates after the oversized frame")
        .expect("the task joins");
    assert!(
        result.is_ok(),
        "clean stop after the overrun, got {result:?}"
    );

    // Exactly one -32700 oversized-frame line, then EOF.
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["error"]["code"], -32700, "{reply}");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(message.contains("exceed"), "{reply}");
    assert!(
        message.contains(&MAX_LINE_BYTES.to_string()),
        "the limit is named: {message}"
    );
    expect_eof(&mut client_out).await;

    // The writer terminated (its writes failed once the reader was gone).
    tokio::time::timeout(TIMEOUT, writer)
        .await
        .expect("the writer stops after the server hung up")
        .expect("the writer joins");
}

// ---------------------------------------------------------------------------
// Bounded reading: a delimited line OVER the limit (the boundary window)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delimited_over_limit_line_is_answered_and_the_loop_continues() {
    let (mut client_in, server_in) = tokio::io::duplex(PIPE);
    let (server_out, client_end) = tokio::io::duplex(PIPE);
    let mut client_out: ClientOut = BufReader::new(client_end).lines();
    let task = tokio::spawn(run_over(server_in, server_out, server()));

    // A line of MAX_LINE_BYTES + 10 bytes WITH its trailing newline: it
    // is assembled inside the reader's memory bound (the frame cap
    // tolerates one reader chunk past the message bound) and rejected at
    // the MESSAGE layer — -32600, defense in depth — with the loop
    // KEEPING READING (unlike the undelimited overrun).
    let oversized = format!("{}\n", "x".repeat(MAX_LINE_BYTES + 10),);
    client_in
        .write_all(oversized.as_bytes())
        .await
        .expect("write the oversized delimited line");

    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["error"]["code"], -32600, "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("too large"),
        "{reply}"
    );

    // The loop is alive: the next line is answered.
    write_line(
        &mut client_in,
        r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#,
    )
    .await;
    let reply = next_reply(&mut client_out).await;
    assert_eq!(reply["id"], 6);
    assert_eq!(reply["result"], json!({}));

    drop(client_in);
    let result = tokio::time::timeout(TIMEOUT, task)
        .await
        .expect("the loop finishes after EOF")
        .expect("the task joins");
    assert!(result.is_ok(), "{result:?}");
}
