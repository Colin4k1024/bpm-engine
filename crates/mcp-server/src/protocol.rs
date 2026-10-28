//! The MCP protocol engine: a JSON-RPC 2.0 message handler implementing the
//! Model Context Protocol over stdio (E4-2, issue #20).
//!
//! The handler is I/O-free by construction: [`McpServer`] is a plain struct
//! holding the allowlisted catalog tools, their flattened input schemas,
//! and an injected [`ToolCaller`] (the production
//! [`RestClient`](crate::http::RestClient) from
//! [`crate::http`], or a test fake). `handle_line`/`handle_message` are
//! async pure functions of the input — the stdio loop in [`crate::stdio`]
//! is the only place that touches real streams, so the whole protocol is
//! testable without spawning a process.
//!
//! Supported methods (the initialize-handshake era that mainstream stdio
//! MCP clients speak):
//!
//! - `initialize` — protocol-version negotiation, capabilities,
//!   `serverInfo`, and `instructions`;
//! - `notifications/initialized` (and every other notification) — accepted
//!   silently, no response, per JSON-RPC;
//! - `tools/list` — the allowlisted catalog tools with flattened
//!   `inputSchema`s and read-only annotations;
//! - `tools/call` — validates arguments, builds the REST request, dispatches
//!   through the caller; REST outcomes become tool results (errors as
//!   `isError: true` carrying the engine's error body — NEVER protocol
//!   errors);
//! - `ping`.
//!
//! Error mapping: unknown method → `-32601`; malformed JSON line →
//! `-32700` (the loop keeps reading); non-object messages, wrong
//! `jsonrpc`, missing `method` → `-32600`; unknown tool, non-object or
//! schema-violating arguments → `-32602`; dispatch invariant breakage →
//! `-32603`. **Notifications (no `id`) are never answered** — the check
//! runs BEFORE every other validation, so even a notification carrying a
//! wrong `jsonrpc` version or a missing `method` stays silent (JSON-RPC:
//! servers MUST NOT reply to notifications); only requests (an `id`
//! present, `"id": null` included) can earn the `-32600` family. The
//! stdio read layer additionally answers its own transport-level
//! rejections (an oversized undelimited frame, undecodable bytes) with
//! `-32700` lines built by [`error_line`].
//! Request ids are echoed verbatim (string | number | null).
//! Responses are compact single-line JSON (MCP forbids embedded newlines).
//! Messages are processed sequentially in arrival order; v1 does not
//! abort in-flight calls on `notifications/cancelled`.

use crate::allowlist;
use crate::catalog::{Catalog, Classification, ToolSpec};
use crate::tools::{self, RestRequest};

use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// A boxed, `Send` future returned by [`ToolCaller::call_tool`].
pub type BoxCallFuture<'a> = Pin<Box<dyn Future<Output = ToolCallOutcome> + Send + 'a>>;

/// The result of executing one tool call: what the MCP tool result
/// carries.
#[derive(Debug, Clone)]
pub struct ToolCallOutcome {
    /// Whether the tool call FAILED at the tool level (REST error status,
    /// timeout, transport failure). Protocol errors never come through
    /// here — see the module docs.
    pub is_error: bool,
    /// The tool-result text content: the engine's (pretty-printed) JSON
    /// response or error body, or a transport-failure description.
    pub text: String,
}

/// The engine-side executor injected into [`McpServer`] — the REST client
/// in production, a fake in tests.
pub trait ToolCaller: Send + Sync {
    /// Execute one dispatchable REST request for `tool_name`.
    fn call_tool<'a>(&'a self, request: RestRequest, tool_name: &'a str) -> BoxCallFuture<'a>;
}

/// Supported protocol versions (the initialize-handshake era).
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
/// The latest supported protocol version (the fallback answer for
/// unknown/newer client versions).
pub const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";
/// The MCP server name reported in `serverInfo`.
pub const SERVER_NAME: &str = "bpm-engine-mcp-server";

// JSON-RPC 2.0 error codes.
/// Parse error (`-32700`); public for the stdio transport layer's
/// frame-level rejections (oversized frames, undecodable bytes), which
/// share this code and the wire shape built by [`error_line`].
pub const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// The static part of the `initialize` instructions (the exposed tools are
/// appended at runtime).
const INSTRUCTIONS: &str = "BPM engine tools over the engine REST API. Read tools are safe to \
call freely; mutating tools change engine state — check each tool's description for its REST \
operation and side effects. Engine errors come back as tool results with isError=true carrying \
the engine's JSON error body (stable `code`). The tool allowlist of this server is:";

/// An MCP server: the allowlisted tool surface plus the injected caller.
pub struct McpServer {
    tools: Vec<ToolSpec>,
    schemas: HashMap<String, Value>,
    caller: Box<dyn ToolCaller>,
}

impl McpServer {
    /// Build a server exposing `tool_names` from the EMBEDDED catalog,
    /// dispatching through `caller`.
    ///
    /// Fails closed: names must exist in the catalog AND be part of the
    /// v1 tool surface ([`allowlist::V1_SURFACE`]), and every tool must
    /// flatten into a valid input schema.
    pub fn new(caller: Box<dyn ToolCaller>, tool_names: &[String]) -> anyhow::Result<Self> {
        let catalog = Catalog::embedded()
            .map_err(|e| anyhow::anyhow!("parsing the embedded agent tool catalog: {e}"))?;
        Self::with_catalog(catalog, caller, tool_names)
    }

    /// Build a server from an explicit catalog (tests / embedders with a
    /// custom catalog source).
    pub fn with_catalog(
        catalog: Catalog,
        caller: Box<dyn ToolCaller>,
        tool_names: &[String],
    ) -> anyhow::Result<Self> {
        allowlist::ensure_v1(tool_names)
            .map_err(|e| anyhow::anyhow!("tool allowlist rejected: {e}"))?;
        let mut tools = Vec::new();
        let mut schemas = HashMap::new();
        for name in tool_names {
            let tool = catalog
                .find(name)
                .ok_or_else(|| anyhow::anyhow!("tool {name:?} not found in the agent catalog"))?
                .clone();
            let schema = tools::input_schema(&tool)
                .map_err(|e| anyhow::anyhow!("flattening tool {name:?}: {e}"))?;
            schemas.insert(name.clone(), schema);
            tools.push(tool);
        }
        Ok(Self {
            tools,
            schemas,
            caller,
        })
    }

    /// The names of the exposed tools (v1 surface order).
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name.as_str()).collect()
    }

    /// Handle one raw stdin line (newline- or CRLF-terminated, already
    /// stripped by the reader).
    ///
    /// Returns the serialized single-line JSON response for requests, or
    /// `None` for notifications / ignorable lines. Malformed input is
    /// answered with `-32700`/`-32600` and NEVER panics; the loop keeps
    /// reading afterwards. The message-size bound ([`crate::stdio`]'s
    /// read layer) is re-checked here as defense in depth.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            return None; // Blank lines are transport noise, not messages.
        }
        if line.len() > crate::stdio::MAX_LINE_BYTES {
            return Some(serialize(&error_response(
                Value::Null,
                INVALID_REQUEST,
                format!(
                    "message too large: {} bytes exceeds the {} byte limit",
                    line.len(),
                    crate::stdio::MAX_LINE_BYTES
                ),
            )));
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(e) => {
                return Some(serialize(&error_response(
                    Value::Null,
                    PARSE_ERROR,
                    format!("parse error: {e}"),
                )))
            }
        };
        if !message.is_object() {
            // A JSON array is a JSON-RPC batch (removed from MCP); any
            // other non-object message is invalid. Both answer -32600.
            return Some(serialize(&error_response(
                Value::Null,
                INVALID_REQUEST,
                "messages must be JSON-RPC 2.0 objects (batching is not part of MCP)".to_string(),
            )));
        }
        self.handle_message(&message)
            .await
            .map(|response| serialize(&response))
    }

    /// Handle one parsed JSON-RPC 2.0 message.
    ///
    /// `Some(response)` for requests, `None` for notifications and
    /// client-to-server responses (a protocol violation that is still
    /// ignored quietly rather than answered — a response to a response
    /// would be meaningless).
    pub async fn handle_message(&self, message: &Value) -> Option<Value> {
        // A message with NO `id` is a NOTIFICATION, and JSON-RPC forbids
        // replying to notifications — so nothing about one (a wrong
        // `jsonrpc` version, a missing or unknown `method`, any other
        // malformation) may produce a response. This runs BEFORE all
        // validation; only requests (an `id` present, `"id": null`
        // included) can earn the -32600 family. (`Value::get` on a
        // non-object is `None`, so non-objects return quietly here too —
        // `handle_line` already answers those with -32600.)
        message.get("id")?;

        // jsonrpc must be exactly "2.0" when present.
        if let Some(version) = message.get("jsonrpc") {
            if version.as_str() != Some("2.0") {
                return Some(error_response(
                    id_or_null(message),
                    INVALID_REQUEST,
                    "jsonrpc must be \"2.0\"".to_string(),
                ));
            }
        }

        // Validate the request id type (string | number | null).
        let id = message.get("id");
        if let Some(id_value) = id {
            if !(id_value.is_string() || id_value.is_number() || id_value.is_null()) {
                return Some(error_response(
                    Value::Null,
                    INVALID_REQUEST,
                    "id must be a string, number, or null".to_string(),
                ));
            }
        }

        let method = match message.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => {
                // No method: a JSON-RPC response (client→server responses
                // are forbidden on stdin) or plain garbage — a message
                // WITHOUT an id already returned as a notification above.
                if message.get("result").is_some() || message.get("error").is_some() {
                    return None; // ignore client responses quietly
                }
                return Some(error_response(
                    id.cloned().unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    "missing method".to_string(),
                ));
            }
        };

        // Notifications (no id) never get a response — including unknown
        // ones, per JSON-RPC.
        let id = id.cloned()?;

        match method {
            "initialize" => Some(self.handle_initialize(message, id)),
            "ping" => Some(response(id, json!({}))),
            "tools/list" => Some(self.handle_tools_list(id)),
            "tools/call" => self.handle_tools_call(message, id).await,
            other => Some(error_response(
                id,
                METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )),
        }
    }

    /// `initialize`: echo a supported protocol version (the client's when
    /// known, else the latest supported), advertise the tools capability,
    /// and describe the server.
    fn handle_initialize(&self, message: &Value, id: Value) -> Value {
        let requested = message
            .get("params")
            .and_then(|params| params.get("protocolVersion"))
            .and_then(Value::as_str);
        let version = match requested {
            Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
            _ => LATEST_PROTOCOL_VERSION,
        };
        response(
            id,
            json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": format!(
                    "{INSTRUCTIONS}\n{}",
                    self.tool_names().join(", ")
                ),
            }),
        )
    }

    /// `tools/list`: the allowlisted catalog tools with flattened
    /// input schemas and annotations.
    fn handle_tools_list(&self, id: Value) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": mcp_description(tool),
                    "inputSchema": self.schemas[&tool.name],
                    "annotations": {
                        "title": tool.summary,
                        "readOnlyHint": tool.classification.is_read_only(),
                    },
                })
            })
            .collect();
        response(id, json!({ "tools": tools }))
    }

    /// `tools/call`: validate arguments, build the REST request, dispatch.
    async fn handle_tools_call(&self, message: &Value, id: Value) -> Option<Value> {
        let params = match message.get("params").and_then(Value::as_object) {
            Some(params) => params,
            None => {
                return Some(error_response(
                    id,
                    INVALID_PARAMS,
                    "tools/call params must be an object".to_string(),
                ))
            }
        };
        let name = match params.get("name").and_then(Value::as_str) {
            Some(name) => name,
            None => {
                return Some(error_response(
                    id,
                    INVALID_PARAMS,
                    "tools/call requires a string tool name".to_string(),
                ))
            }
        };
        // A missing or null `arguments` is an empty object.
        let arguments = params
            .get("arguments")
            .cloned()
            .filter(|value| !value.is_null())
            .unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Some(error_response(
                id,
                INVALID_PARAMS,
                "arguments must be a JSON object".to_string(),
            ));
        }
        let Some(tool) = self.tools.iter().find(|tool| tool.name == name) else {
            // Unknown (or not allowlisted) tools are protocol errors.
            return Some(error_response(
                id,
                INVALID_PARAMS,
                format!("unknown tool {name:?}: not exposed by this server (see tools/list)"),
            ));
        };
        if let Err(message) = tools::validate_arguments(tool, &arguments) {
            return Some(error_response(id, INVALID_PARAMS, message));
        }
        let request = match tools::build_request(tool, &arguments) {
            Ok(request) => request,
            Err(message) => {
                // Post-validation invariant breakage.
                return Some(error_response(
                    id,
                    INTERNAL_ERROR,
                    format!("building the REST request for {name:?} failed: {message}"),
                ));
            }
        };

        let outcome = self.caller.call_tool(request, &tool.name).await;
        Some(response(
            id,
            json!({
                "content": [ { "type": "text", "text": outcome.text } ],
                "isError": outcome.is_error,
            }),
        ))
    }
}

/// Build the MCP tool description: the catalog summary, the long
/// description, and a machine-derived REST line (operation,
/// classification, side effects) so agents see what each tool does on the
/// wire.
fn mcp_description(tool: &ToolSpec) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(tool.summary.clone());
    if let Some(description) = &tool.description {
        parts.push(description.clone());
    }
    match tool.classification {
        Classification::ReadOnly => parts.push(format!(
            "REST {} (read-only: no engine state changes).",
            tool.operation
        )),
        Classification::Mutating => parts.push(format!(
            "REST {} (mutating — side effects: {}).",
            tool.operation,
            tool.side_effects
                .as_deref()
                .unwrap_or("changes engine state")
        )),
    }
    parts.join("\n\n")
}

/// A JSON-RPC 2.0 success response.
fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC 2.0 error response.
fn error_response(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The message's `id`, or null when absent.
fn id_or_null(message: &Value) -> Value {
    message.get("id").cloned().unwrap_or(Value::Null)
}

/// Serialize a response as ONE compact line (MCP messages must not contain
/// embedded newlines).
fn serialize(response: &Value) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| {
        "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"response serialization failed\"}}"
            .to_string()
    })
}

/// A serialized single-line JSON-RPC error response with a null id, for
/// TRANSPORT-level rejections that happen before a message (and its
/// request id) exists: the stdio read layer's oversized-frame and
/// invalid-UTF-8 answers. Anything that parsed as JSON goes through
/// [`McpServer::handle_line`] instead.
pub fn error_line(code: i64, message: String) -> String {
    serialize(&error_response(Value::Null, code, message))
}
