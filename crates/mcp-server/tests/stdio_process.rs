//! Subprocess tests for the MCP server binary (E4-2): the REAL stdio
//! wiring — the actual `bpm-engine-mcp-server` process (built by cargo as
//! `CARGO_BIN_EXE_bpm-engine-mcp-server`), spawned with piped
//! stdin/stdout and driven over the true OS pipes, against a REAL
//! in-process engine (`build_app` memory mode on an ephemeral loopback
//! port — the same harness as `tests/end_to_end.rs`).
//!
//! Covered:
//! - a full session over the pipes: the initialize handshake, a
//!   notification (no reply), `tools/list`, a real `tools/call` returning
//!   engine data, a malformed line MID-SESSION (`-32700`, then the loop
//!   keeps reading), and — after closing stdin — a clean exit with code
//!   0 (the EOF path, the MCP shutdown signal);
//! - the read-layer bound END TO END: streaming ~9 MiB WITHOUT a newline
//!   makes the process answer exactly one `-32700` oversized-frame line
//!   and terminate cleanly — a client streaming undelimited bytes cannot
//!   grow the process's memory without bound.
//!
//! Everything is timeout-bounded (a hang fails the test rather than
//! blocking CI); `kill_on_drop` ensures a panicking test leaks no child.

use bpm_engine_mcp_server::allowlist;
use bpm_engine_server_rest::bpm_config::{
    AuthConfig, BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::sync::CancellationToken;

/// Fixed admin key for the in-process engine (the test server config).
const TEST_API_KEY: &str = "mcp-stdio-admin-key";
/// Bound for every single read from the child's stdout.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound for waiting on the child's exit.
const EXIT_TIMEOUT: Duration = Duration::from_secs(15);
/// The child's stdout, one reply line at a time.
type ChildOut = Lines<BufReader<ChildStdout>>;

// ---------------------------------------------------------------------------
// The in-process engine (same harness as tests/end_to_end.rs)
// ---------------------------------------------------------------------------

struct EngineServer {
    base_url: String,
    serve_task: tokio::task::JoinHandle<()>,
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
            jwt_secret: "mcp-stdio-secret".to_string(),
            api_key: TEST_API_KEY.to_string(),
        },
        rate_limit: RateLimitConfig {
            requests_per_minute: 1_000_000,
        },
        ..Default::default()
    }
}

/// A real `build_app` memory-mode engine on an ephemeral loopback port.
async fn spawn_engine() -> EngineServer {
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
    EngineServer {
        base_url: format!("http://{addr}"),
        serve_task,
        timer_cancel,
    }
}

impl EngineServer {
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// The MCP server subprocess
// ---------------------------------------------------------------------------

/// Spawn the real binary against `base_url`, authenticated with the
/// engine's admin API key (loopback http + a credential is allowed —
/// Finding 5's guard only refuses NON-loopback plaintext).
fn spawn_mcp(base_url: &str) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bpm-engine-mcp-server"));
    // Scrub inherited BPM_* env vars (e.g. the test harness's
    // BPM_TEST_DATABASE_URL) so the child is driven ONLY by the
    // variables set here.
    for (key, _) in std::env::vars() {
        if key.starts_with("BPM_") {
            cmd.env_remove(&key);
        }
    }
    cmd.env("BPM_BASE_URL", base_url)
        .env("BPM_MCP_API_KEY", TEST_API_KEY)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // A panicking test must not leak the child process.
        .kill_on_drop(true)
        .spawn()
        .expect("spawn bpm-engine-mcp-server")
}

/// Write one newline-terminated line to the child's stdin.
async fn write_line(stdin: &mut ChildStdin, line: &str) {
    stdin.write_all(line.as_bytes()).await.expect("write line");
    stdin.write_all(b"\n").await.expect("write newline");
    stdin.flush().await.expect("flush stdin");
}

/// Read the next reply line from the child's stdout, parsed (bounded).
async fn read_reply(stdout: &mut ChildOut) -> Value {
    let line = tokio::time::timeout(READ_TIMEOUT, stdout.next_line())
        .await
        .expect("a reply within the timeout")
        .expect("stdout stays open")
        .expect("a reply line, not EOF");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply {line:?} is JSON: {e}"))
}

// ---------------------------------------------------------------------------
// A full session over the real pipes, ending with the EOF exit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subprocess_session_over_real_pipes_exits_cleanly_on_eof() {
    let engine = spawn_engine().await;
    let mut child = spawn_mcp(&engine.base_url);
    let mut stdin: ChildStdin = child.stdin.take().expect("stdin piped");
    let mut stdout: ChildOut = BufReader::new(child.stdout.take().expect("stdout piped")).lines();

    // Handshake over the real pipe.
    write_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    )
    .await;
    let reply = read_reply(&mut stdout).await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        "bpm-engine-mcp-server"
    );

    // A notification: NO reply (the next reply must belong to the next
    // REQUEST — the id pinning proves the ordering).
    write_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;

    // tools/list: the default read-only surface, exactly.
    write_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )
    .await;
    let reply = read_reply(&mut stdout).await;
    assert_eq!(reply["id"], 2, "the notification stayed silent: {reply}");
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(names, allowlist::DEFAULT_TOOLS.to_vec());

    // A real tool call through the real binary: the engine's data.
    write_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_process_definitions","arguments":{}}}"#,
    )
    .await;
    let reply = read_reply(&mut stdout).await;
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    assert!(
        text.contains("minimal"),
        "engine data in the tool result: {text}"
    );

    // A malformed line MID-SESSION: -32700, and the loop KEEPS READING —
    // the very next call succeeds.
    write_line(&mut stdin, "{oops").await;
    let reply = read_reply(&mut stdout).await;
    assert_eq!(reply["error"]["code"], -32700);
    write_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_capabilities","arguments":{}}}"#,
    )
    .await;
    let reply = read_reply(&mut stdout).await;
    assert_eq!(reply["id"], 5);
    assert_eq!(reply["result"]["isError"], false, "{reply}");

    // Close stdin — the MCP shutdown signal: the process must exit
    // cleanly (code 0), proving the EOF path.
    drop(stdin);
    let status = tokio::time::timeout(EXIT_TIMEOUT, child.wait())
        .await
        .expect("exit within the timeout")
        .expect("wait for the child");
    assert!(status.success(), "clean EOF exit, got {status}");

    engine.stop().await;
}

// ---------------------------------------------------------------------------
// The read-layer bound, end to end: an undelimited stream terminates
// the process after exactly one -32700 line
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subprocess_oversized_undelimited_stream_terminates() {
    // No engine needed: the read-layer bound fires before any dispatch
    // (loopback http with no credential is unconditionally allowed).
    let mut child = spawn_mcp("http://127.0.0.1:1");
    let stdin: ChildStdin = child.stdin.take().expect("stdin piped");
    let mut stdout: ChildOut = BufReader::new(child.stdout.take().expect("stdout piped")).lines();

    // ~9 MiB of 'a' with NO newline — past the 8 MiB message bound. The
    // child stops reading once the frame is oversized, answers once,
    // and terminates; this writer then sees a closed pipe (EPIPE) and
    // stops. Streaming 9 MiB through a pipe is fast (< 1 s).
    let writer = tokio::spawn(async move {
        let mut stdin = stdin;
        let chunk = [b'a'; 64 * 1024];
        let mut remaining = 9 * 1024 * 1024;
        while remaining > 0 {
            let want = chunk.len().min(remaining);
            match stdin.write(&chunk[..want]).await {
                Ok(0) | Err(_) => break, // the child hung up: frame oversized
                Ok(n) => remaining -= n,
            }
        }
    });

    // Exactly ONE -32700 oversized-frame line naming the limit.
    let reply = read_reply(&mut stdout).await;
    assert_eq!(reply["error"]["code"], -32700, "{reply}");
    let message = reply["error"]["message"].as_str().expect("message");
    assert!(message.contains("exceed"), "{reply}");
    assert!(
        message.contains("8388608"),
        "the byte limit is named: {message}"
    );

    // The process TERMINATES — cleanly (it closed the stream on purpose).
    let status = tokio::time::timeout(EXIT_TIMEOUT, child.wait())
        .await
        .expect("exit within the timeout")
        .expect("wait for the child");
    assert!(status.success(), "clean termination, got {status}");

    // Nothing further on stdout: exactly one error line was written.
    let line = tokio::time::timeout(READ_TIMEOUT, stdout.next_line())
        .await
        .expect("stdout drains within the timeout")
        .expect("stdout closed after the child exited");
    assert!(line.is_none(), "no output after the error line: {line:?}");

    // The writer stopped when the child hung up.
    tokio::time::timeout(EXIT_TIMEOUT, writer)
        .await
        .expect("the writer stops after the child hung up")
        .expect("the writer joins");
}
