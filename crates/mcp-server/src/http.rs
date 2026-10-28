//! The engine REST caller: the production [`ToolCaller`] that turns a
//! built [`RestRequest`] into one authenticated HTTP call against
//! `BPM_BASE_URL` (loopback by default) and renders the outcome as an MCP
//! tool result.
//!
//! Semantics:
//!
//! - every call carries the configured credential ([`Credential::Bearer`]
//!   → `Authorization: Bearer …`, [`Credential::ApiKey`] → `X-API-Key`,
//!   open mode → nothing) and the per-request `Idempotency-Key` when the
//!   tool arguments supplied one;
//! - every call is bounded by the configured REST timeout (default 10 s) —
//!   a timeout or transport failure becomes a tool result with
//!   `isError: true`, never a protocol error and never a hang;
//! - the HTTP status decides `isError`: 2xx → success, anything else →
//!   error, with the engine's JSON error body (`{error, code,
//!   invariant_violation?}`) carried VERBATIM in the text content (the
//!   stable `code` values are the agent-facing contract, E4-1);
//! - 2xx bodies are pretty-printed JSON when parseable (readable for
//!   agents) and raw text otherwise; empty bodies (204) render as the
//!   status line;
//! - no secret is ever logged or included in a tool result.

use crate::config::{Credential, McpConfig};
use crate::protocol::{BoxCallFuture, ToolCallOutcome, ToolCaller};
use crate::tools::{RestBody, RestRequest};
use serde_json::Value;

/// A pooled HTTP client for the engine REST API.
#[derive(Clone)]
pub struct RestClient {
    http: reqwest::Client,
    /// Engine base URL, no trailing slash.
    base_url: String,
    credential: Credential,
}

impl RestClient {
    /// Build the client from configuration. Errors when the HTTP client
    /// itself cannot be built (exotic TLS-init failures).
    ///
    /// Proxies are disabled: the engine is reached directly (loopback by
    /// default), and routing engine credentials through an ambient
    /// `HTTP_PROXY` would both leak them and break loopback calls.
    pub fn new(config: &McpConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(config.rest_timeout)
            .no_proxy()
            .build()?;
        Ok(Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            credential: config.credential.clone(),
        })
    }

    /// Execute one request and render the tool-call outcome.
    async fn execute(&self, request: RestRequest) -> Result<ToolCallOutcome, String> {
        let url = format!("{}{}", self.base_url, request.path);
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|e| format!("invalid REST method {:?}: {e}", request.method))?;
        let mut builder = self.http.request(method, &url);

        builder = match &self.credential {
            Credential::Bearer(token) => builder.bearer_auth(token),
            Credential::ApiKey(key) => builder.header("x-api-key", key.as_str()),
            Credential::None => builder,
        };
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if !request.query.is_empty() {
            builder = builder.query(&request.query);
        }
        builder = match &request.body {
            Some(RestBody::Json(value)) => builder.json(value),
            Some(RestBody::Raw { content_type, data }) => builder
                .header(reqwest::header::CONTENT_TYPE, content_type.as_str())
                .body(data.clone()),
            None => builder,
        };

        let response = builder.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        let body = response.text().await.map_err(|e| e.to_string())?;
        Ok(ToolCallOutcome {
            is_error: !status.is_success(),
            text: render_body(&body, status),
        })
    }
}

impl ToolCaller for RestClient {
    fn call_tool<'a>(&'a self, request: RestRequest, tool_name: &'a str) -> BoxCallFuture<'a> {
        Box::pin(async move {
            match self.execute(request).await {
                Ok(outcome) => outcome,
                Err(message) => ToolCallOutcome {
                    is_error: true,
                    // Transport/timeout failure: the tool call failed
                    // before the engine answered (no secret is included).
                    text: format!("BPM REST call for tool {tool_name:?} failed: {message}"),
                },
            }
        })
    }
}

/// Render an engine response body as tool-result text.
///
/// Valid JSON is pretty-printed (success and error bodies alike — the
/// error body carries the stable `code`); anything else passes through
/// verbatim; an empty body renders as the HTTP status line.
fn render_body(body: &str, status: reqwest::StatusCode) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return format!("HTTP {status}");
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| trimmed.to_string()),
        Err(_) => trimmed.to_string(),
    }
}
