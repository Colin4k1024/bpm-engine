//! `bpm-engine-mcp-server` — a Model Context Protocol (MCP) server that
//! exposes the BPM engine's agent tool surface (E4-1 catalog) over stdio
//! JSON-RPC 2.0, dispatching every tool call to the engine's REST API
//! (E4-2, issue #20).
//!
//! # What it does
//!
//! - Speaks MCP over stdin/stdout (newline-delimited JSON-RPC 2.0):
//!   `initialize`, `notifications/initialized`, `tools/list`,
//!   `tools/call`, `ping`.
//! - Serves the E4-1 agent tool catalog — the SAME committed artifact the
//!   REST server embeds (`crates/server/rest/capabilities/agent-tools.json`,
//!   included here via a workspace-relative [`include_str!`] so there is
//!   no copy) — filtered to the **v1 tool surface**: definition
//!   validate/dry-run/deploy/version listing, instance
//!   start/query/history/steps, the external-task lease lifecycle,
//!   user-task list/complete, and dead-letter list/get/requeue.
//! - Enforces **minimal permission by allowlist** ([`allowlist`]): the
//!   default exposure is the read-only surface; mutating tools require a
//!   role preset (`BPM_MCP_ROLE`) or an explicit `BPM_MCP_ALLOWED_TOOLS`
//!   list, both bounded by the v1 surface and failing closed on typos.
//! - Adds **no trust of its own** ([`config`]): it forwards one
//!   pre-provisioned credential (`BPM_MCP_TOKEN` bearer JWT preferred, or
//!   the legacy `BPM_MCP_API_KEY`) on every engine call, never sets
//!   `X-Tenant-Id`, and never lets a tool argument smuggle a header —
//!   tenancy and RBAC are verified engine-side (E5-1) from the credential
//!   claims; engine 401/403/404 bodies surface verbatim in tool results.
//!
//! # Running
//!
//! ```sh
//! BPM_BASE_URL=http://127.0.0.1:3000 \
//! BPM_MCP_TOKEN=$(cargo run -p bpm-engine-server-rest --example mint_jwt -- \
//!     --sub mcp --tenant acme --role operator) \
//! cargo run -p bpm-engine-mcp-server
//! ```
//!
//! Point any stdio MCP client at the binary. See `docs/mcp-server.md`
//! for the full protocol, configuration, and permission model.

pub mod allowlist;
pub mod catalog;
pub mod config;
pub mod http;
pub mod protocol;
pub mod stdio;
pub mod tools;

pub use catalog::{Catalog, Classification, ToolSpec, AGENT_TOOLS_JSON};
pub use config::{Credential, McpConfig, DEFAULT_BASE_URL, DEFAULT_REST_TIMEOUT};
pub use protocol::{BoxCallFuture, McpServer, ToolCallOutcome, ToolCaller, SERVER_NAME};
pub use tools::{build_request, flatten, input_schema, validate_arguments, RestBody, RestRequest};
