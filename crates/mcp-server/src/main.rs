//! The `bpm-engine-mcp-server` binary: build the configuration and tool
//! allowlist from the environment, announce the setup on stderr (stdout is
//! the MCP protocol channel — nothing else may be written there), and run
//! the stdio loop until the client closes stdin.
//!
//! Configuration errors abort startup with a message naming the remedies:
//! a typo'd `BPM_MCP_ALLOWED_TOOLS` entry, an unknown `BPM_MCP_ROLE` (the
//! valid values are listed), or a credential that would be sent in
//! cleartext over non-loopback `http://` (use https, a loopback host, or
//! the explicit `BPM_MCP_ALLOW_INSECURE_HTTP=1` opt-in) — permission and
//! transport boundaries fail closed. Secrets are never printed.

use bpm_engine_mcp_server::config::McpConfig;
use bpm_engine_mcp_server::http::RestClient;
use bpm_engine_mcp_server::protocol::{McpServer, SERVER_NAME};
use bpm_engine_mcp_server::stdio::run_stdio;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = McpConfig::from_env()?;
    let tool_names = bpm_engine_mcp_server::allowlist::resolve_from_env()
        .map_err(|e| anyhow::anyhow!("resolving the tool allowlist: {e}"))?;

    // Startup banner: stderr only (stdout is reserved for MCP messages).
    eprintln!(
        "{SERVER_NAME} {} (MCP over stdio)",
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
    run_stdio(server).await
}
