//! The `bpm-engine-mcp-server` binary: build the configuration and tool
//! allowlist from the environment, announce the setup on stderr (stdout is
//! the MCP protocol channel in stdio mode — nothing else may be written
//! there), and run the selected transport until the client closes stdin
//! (stdio) or the process is interrupted (HTTP+SSE).
//!
//! Configuration errors abort startup with a message naming the remedies:
//! a typo'd `BPM_MCP_ALLOWED_TOOLS` entry, an unknown `BPM_MCP_ROLE` (the
//! valid values are listed), a credential that would be sent in cleartext
//! over non-loopback `http://` (use https, a loopback host, or the
//! explicit `BPM_MCP_ALLOW_INSECURE_HTTP=1` opt-in), or an unknown
//! `BPM_MCP_TRANSPORT` (valid: `stdio`, `http`) — permission and transport
//! boundaries fail closed. Secrets are never printed.

use bpm_engine_mcp_server::config::McpConfig;
use bpm_engine_mcp_server::http::RestClient;
use bpm_engine_mcp_server::protocol::{McpServer, SERVER_NAME};
use bpm_engine_mcp_server::stdio::run_stdio;
use bpm_engine_mcp_server::transport_http::run_http;

/// The default MCP transport (stdio — the subprocess model mainstream MCP
/// clients use).
const DEFAULT_TRANSPORT: &str = "stdio";
/// The default HTTP+SSE bind address (loopback only; the HTTP transport
/// adds a listening surface, so loopback is the safe default — bind a
/// non-loopback address only behind a reverse proxy that handles auth/TLS).
const DEFAULT_HTTP_BIND: &str = "127.0.0.1:9100";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = McpConfig::from_env()?;
    let tool_names = bpm_engine_mcp_server::allowlist::resolve_from_env()
        .map_err(|e| anyhow::anyhow!("resolving the tool allowlist: {e}"))?;

    let transport = read_env("BPM_MCP_TRANSPORT").unwrap_or_else(|| DEFAULT_TRANSPORT.to_string());
    let http_bind = read_env("BPM_MCP_HTTP_BIND").unwrap_or_else(|| DEFAULT_HTTP_BIND.to_string());

    // Startup banner: stderr only. In stdio mode stdout is reserved for MCP
    // messages; in HTTP mode stdout is unused but stderr stays the log sink.
    eprintln!(
        "{SERVER_NAME} {} (MCP over {transport})",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!("engine REST base URL: {}", config.base_url);
    match &config.credential {
        bpm_engine_mcp_server::config::Credential::Bearer(_) => {
            eprintln!(
                "credential: bearer token (BPM_MCP_TOKEN); tenant/role are verified engine-side"
            );
        }
        bpm_engine_mcp_server::config::Credential::ApiKey(_) => {
            eprintln!("credential: admin API key (BPM_MCP_API_KEY) — full engine access; prefer a role-scoped BPM_MCP_TOKEN");
        }
        bpm_engine_mcp_server::config::Credential::None => {
            eprintln!(
                "credential: NONE (open mode) — no credential is attached; only safe for \
                 local development against an engine in open mode"
            );
        }
    }
    eprintln!(
        "engine REST timeout: {} ms",
        config.rest_timeout.as_millis()
    );
    eprintln!(
        "tools exposed: {} of the v1 surface: {}",
        tool_names.len(),
        tool_names.join(", ")
    );
    eprintln!(
        "tenant policy: X-Tenant-Id is never set or settable through this server; the engine \
         derives the tenant from the credential's verified claim (E5-1)"
    );

    let caller = RestClient::new(&config)?;
    let server = McpServer::new(Box::new(caller), &tool_names)?;

    match transport.as_str() {
        "stdio" => run_stdio(server).await,
        "http" => {
            eprintln!("HTTP+SSE bind: {http_bind} (loopback by default; GET /sse, POST /messages)");
            run_http(server, &http_bind).await
        }
        other => anyhow::bail!(
            "unknown BPM_MCP_TRANSPORT {other:?}: expected \"stdio\" (default) or \"http\""
        ),
    }
}

/// Read a non-empty env var (trimmed), `None` when unset/blank.
fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
