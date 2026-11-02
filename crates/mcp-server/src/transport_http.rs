//! The MCP HTTP+SSE transport (N5-1): an alternative to the stdio loop for
//! network-attached MCP clients that speak the HTTP+SSE transport (one
//! long-lived SSE stream for server→client, POST for client→server).
//!
//! # Flow
//!
//! 1. The client opens `GET /sse`. The server mints a session (an
//!    [`mpsc`] channel keyed by a fresh session id), emits an `endpoint`
//!    SSE event whose data is the POST URL
//!    (`/messages?session_id=<id>`), and holds the connection open,
//!    streaming `message` events.
//! 2. The client POSTs a JSON-RPC 2.0 message to
//!    `/messages?session_id=<id>`. The server runs it through
//!    [`McpServer::handle_message`]; the response (if any — notifications
//!    get none, per JSON-RPC) is emitted as a `message` SSE event on that
//!    client's stream. The POST itself answers `202 Accepted` with an
//!    empty body: the JSON-RPC reply travels over SSE, not the HTTP
//!    response.
//! 3. When the client disconnects from `/sse`, the response future is
//!    dropped, which drops the [`GuardedStream`] and removes the session
//!    from the registry — abandoned sessions do not accumulate even if no
//!    further POST ever arrives.
//!
//! The [`McpServer`] is shared read-only across handlers via
//! [`Arc`](std::sync::Arc); the live sessions live behind a
//! [`Mutex`](std::sync::Mutex) on a `HashMap`. Each session has its own
//! bounded channel, so a slow SSE reader applies backpressure to its own
//! POSTs without stalling any other session.
//!
//! This transport is selected with `BPM_MCP_TRANSPORT=http`
//! (`BPM_MCP_HTTP_BIND` sets the bind address, default `127.0.0.1:9100`).

use crate::protocol::McpServer;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt as _};

/// Per-session SSE channel capacity: responses buffered before a slow SSE
/// reader applies backpressure to its own POSTs. Plenty for one-response-
/// per-request traffic; the POST side `await`s the send, so a full client
/// is naturally paced.
const SSE_CHANNEL_CAPACITY: usize = 32;

/// Shared application state: the MCP server (immutable, shared across all
/// handlers) and the live SSE sessions keyed by id.
#[derive(Clone)]
struct AppState {
    server: Arc<McpServer>,
    sessions: Arc<Mutex<HashMap<String, SessionSender>>>,
}

/// A session's outbound channel — POSTed responses go in here, the SSE
/// stream drains them as `message` events.
type SessionSender = mpsc::Sender<Value>;

/// Run the MCP HTTP+SSE transport until interrupted (Ctrl-C) or the server
/// stops accepting. `bind` is the `host:port` string (e.g.
/// `127.0.0.1:9100`). This is the entry point used by the binary; embedders
/// and tests that want to bind their own (ephemeral) listener and control
/// the lifetime should use [`serve_on`].
pub async fn run_http(server: McpServer, bind: &str) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    let app = build_router(Arc::new(server));
    eprintln!(
        "bpm-engine-mcp-server: HTTP+SSE transport listening on http://{addr} \
         (GET /sse → endpoint event; POST /messages?session_id=…)"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server stopped: {e}"))?;
    Ok(())
}

/// Serve the MCP HTTP+SSE transport on an already-bound listener. Intended
/// for embedders and the in-process test harness: no graceful-shutdown
/// signal is installed, so the caller controls the lifetime (drop the
/// future or abort the task to stop).
pub async fn serve_on(server: McpServer, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let app = build_router(Arc::new(server));
    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("HTTP server stopped: {e}"))?;
    Ok(())
}

/// Build the axum router for the MCP HTTP+SSE transport.
fn build_router(server: Arc<McpServer>) -> Router {
    let state = AppState {
        server,
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    Router::new()
        .route("/sse", get(sse_handler))
        .route("/messages", post(message_handler))
        .with_state(state)
}

/// Ctrl-C → graceful shutdown (in-flight SSE streams close, sessions drop).
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("installing the Ctrl-C handler");
}

/// The query parameters accepted by the endpoints.
#[derive(Deserialize)]
struct SessionQuery {
    /// The session id issued in the `endpoint` SSE event. Required on
    /// `/messages` (to route the response to the right SSE stream);
    /// ignored on `/sse`, which always mints a fresh session.
    session_id: Option<String>,
}

/// `GET /sse`: open the SSE stream. Mints a fresh session id, registers an
/// mpsc sender, emits the `endpoint` event naming the POST URL, then
/// streams `message` events as responses arrive from POSTed requests.
/// Disconnecting the SSE stream drops the [`GuardedStream`], which removes
/// the session from the registry.
async fn sse_handler(State(state): State<AppState>) -> Response {
    let session_id = new_session_id();
    let (tx, rx) = mpsc::channel::<Value>(SSE_CHANNEL_CAPACITY);
    state
        .sessions
        .lock()
        .expect("sessions map not poisoned")
        .insert(session_id.clone(), tx);

    let post_url = format!("/messages?session_id={session_id}");
    let endpoint_event = Ok::<_, Infallible>(Event::default().event("endpoint").data(post_url));
    let messages = ReceiverStream::new(rx).map(|value| {
        Ok::<_, Infallible>(
            Event::default()
                .event("message")
                .data(serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())),
        )
    });
    // The combined stream is Send + Unpin (iter + ReceiverStream + an empty
    // closure), so it boxes as dyn Stream + Send + Unpin without unsafe pin
    // projection: GuardedStream ends up Unpin and get_mut suffices.
    let stream: Box<dyn Stream<Item = Result<Event, Infallible>> + Send + Unpin> =
        Box::new(tokio_stream::iter(vec![endpoint_event]).chain(messages));

    let guarded = GuardedStream {
        inner: stream,
        cleanup: Some(SessionCleanup {
            sessions: state.sessions.clone(),
            session_id,
        }),
    };
    Sse::new(guarded).into_response()
}

/// `POST /messages?session_id=<id>`: process one JSON-RPC message. The body
/// is the JSON-RPC message itself. Responses (requests only — notifications
/// produce none, per JSON-RPC) are emitted on the session's SSE stream; the
/// HTTP response is `202 Accepted` with an empty body (the JSON-RPC reply
/// travels over SSE). A missing session id answers `400`; an unknown /
/// expired one answers `404`; a body that is not valid JSON answers `400`
/// (the [`Json`] extractor).
async fn message_handler(
    State(state): State<AppState>,
    Query(query): Query<SessionQuery>,
    Json(message): Json<Value>,
) -> Response {
    let Some(session_id) = query.session_id else {
        return (
            StatusCode::BAD_REQUEST,
            "missing session_id query parameter (open GET /sse first)",
        )
            .into_response();
    };
    // Clone the sender and drop the guard BEFORE awaiting, so no lock is
    // held across the dispatch (clippy::await_holding_lock) and concurrent
    // POSTs to other sessions are never serialized on the registry.
    let sender = {
        let sessions = state.sessions.lock().expect("sessions map not poisoned");
        match sessions.get(&session_id) {
            Some(sender) => sender.clone(),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    "unknown or expired session_id (open GET /sse first)",
                )
                    .into_response();
            }
        }
    };
    // Notifications return None and emit no SSE event; requests emit the
    // JSON-RPC reply as a `message` event on the caller's SSE stream.
    if let Some(response) = state.server.handle_message(&message).await {
        if sender.send(response).await.is_err() {
            // The SSE stream is gone (client disconnected): drop the stale
            // session so the registry cannot grow without bound. The
            // GuardedStream Drop also cleans up; this covers the race where
            // the POST arrives just as the stream ends.
            state
                .sessions
                .lock()
                .expect("sessions map not poisoned")
                .remove(&session_id);
            return (StatusCode::GONE, "session stream closed").into_response();
        }
    }
    StatusCode::ACCEPTED.into_response()
}

/// Monotonic session id generator (process-local; restart resets it, which
/// is fine — sessions never outlive the process).
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
fn new_session_id() -> String {
    let n = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("sess-{n}")
}

/// Removes a session from the registry when its SSE stream ends.
struct SessionCleanup {
    sessions: Arc<Mutex<HashMap<String, SessionSender>>>,
    session_id: String,
}

impl SessionCleanup {
    fn fire(self) {
        self.sessions
            .lock()
            .expect("sessions map not poisoned")
            .remove(&self.session_id);
    }
}

/// A stream wrapper that runs [`SessionCleanup`] on Drop, so a client
/// disconnecting from `/sse` removes its session even if no further POST
/// ever arrives. The inner stream is boxed and `Unpin`, so the wrapper is
/// `Unpin` too — `poll_next` uses `get_mut` and needs no unsafe pin
/// projection.
struct GuardedStream {
    inner: Box<dyn Stream<Item = Result<Event, Infallible>> + Send + Unpin>,
    cleanup: Option<SessionCleanup>,
}

impl Stream for GuardedStream {
    type Item = Result<Event, Infallible>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // GuardedStream is Unpin (Box<…+Unpin> and Option<…> are both
        // Unpin), so get_mut is sound and trivial — no projection.
        let this = self.get_mut();
        Pin::new(this.inner.as_mut()).poll_next(cx)
    }
}

impl Drop for GuardedStream {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup.fire();
        }
    }
}
