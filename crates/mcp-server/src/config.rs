//! Environment configuration of the MCP server (issue #20: SIMPLEST SOUND
//! v1 — a raw credential plus a base URL; no secret handling beyond
//! forwarding).
//!
//! # Environment variables
//!
//! | Variable | Meaning | Default |
//! | --- | --- | --- |
//! | `BPM_BASE_URL` | Base URL of the engine REST API (loopback by default) | `http://127.0.0.1:3000` |
//! | `BPM_MCP_TOKEN` | Raw bearer credential sent as `Authorization: Bearer …` — a JWT minted with `cargo run -p bpm-engine-server-rest --example mint_jwt` (role-scoped, preferred) | none (open mode) |
//! | `BPM_MCP_API_KEY` | Legacy admin bootstrap key sent as `X-API-Key` (full access — use a role-scoped `BPM_MCP_TOKEN` instead) | none |
//! | `BPM_MCP_TIMEOUT_MS` | Timeout for every engine REST call | `10000` |
//! | `BPM_MCP_ROLE` | Role preset for the tool allowlist (see [`crate::allowlist`]) | none |
//! | `BPM_MCP_ALLOWED_TOOLS` | Explicit tool allowlist (see [`crate::allowlist`]) | none |
//! | `BPM_MCP_ALLOW_INSECURE_HTTP` | Explicit opt-in to send a credential over plaintext non-loopback `http://` (prints a warning; dangerous) | unset — the combination refuses to start |
//!
//! When both `BPM_MCP_TOKEN` and `BPM_MCP_API_KEY` are set the token wins
//! (a presented bearer token also takes precedence engine-side, E5-1).
//! With NEITHER set the server runs in OPEN MODE: it attaches no
//! credential and relies on the engine being in open mode too — local
//! development only, never production (documented in `docs/mcp-server.md`).
//!
//! **Cleartext-credential guard (fail closed)**: a configured credential
//! combined with a NON-loopback `http://` base URL would put the secret
//! on the wire in cleartext — startup REFUSES with an error naming the
//! remedies (an `https://` base URL, a loopback host, or the explicit
//! `BPM_MCP_ALLOW_INSECURE_HTTP=1` opt-in, which admits the combination
//! with a warning on stderr). `https://` anywhere and plaintext loopback
//! (`localhost` and the loopback IP ranges) are always allowed; open mode
//! (no credential) is unaffected.
//!
//! The tenant boundary: this server NEVER sets `X-Tenant-Id` and no tool
//! argument can set it — the engine derives the tenant from the VERIFIED
//! claim of the forwarded credential (E5-1) and rejects cross-tenant
//! access with 404. The MCP layer adds no trust of its own.

use std::time::Duration;

/// Default engine base URL (the REST server's default bind).
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3000";
/// Default per-call REST timeout (10 s, per the robustness requirements).
pub const DEFAULT_REST_TIMEOUT: Duration = Duration::from_secs(10);
/// The explicit opt-in for sending a credential over plaintext
/// non-loopback `http://` (exactly `1`; any other value does NOT override
/// — see [`McpConfig::from_env`]).
pub const ALLOW_INSECURE_HTTP_ENV: &str = "BPM_MCP_ALLOW_INSECURE_HTTP";

/// The credential the MCP server presents to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// A raw bearer token (`Authorization: Bearer …`), typically a
    /// role-scoped JWT minted with the server's `mint_jwt` example.
    Bearer(String),
    /// The legacy admin API key (`X-API-Key`) — full access.
    ApiKey(String),
    /// No credential (open mode — local development only).
    None,
}

/// MCP server configuration (engine REST access).
#[derive(Debug, Clone)]
pub struct McpConfig {
    /// Engine REST base URL (no trailing slash).
    pub base_url: String,
    /// Credential forwarded on every engine REST call.
    pub credential: Credential,
    /// Timeout applied to every engine REST call.
    pub rest_timeout: Duration,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            credential: Credential::None,
            rest_timeout: DEFAULT_REST_TIMEOUT,
        }
    }
}

impl McpConfig {
    /// Build the configuration from the environment (see the module docs
    /// for the variables). A malformed `BPM_MCP_TIMEOUT_MS` falls back to
    /// the default with a warning on stderr, and credential validity is
    /// the engine's to judge (a bad token surfaces as 401 tool errors,
    /// not as startup failure). One thing DOES refuse to start: a
    /// credential that would be sent in cleartext — a non-loopback
    /// `http://` base URL together with `BPM_MCP_TOKEN` /
    /// `BPM_MCP_API_KEY` fails closed (see
    /// [`ALLOW_INSECURE_HTTP_ENV`] for the explicit opt-in).
    pub fn from_env() -> anyhow::Result<Self> {
        let base_url = read_env("BPM_BASE_URL")
            .map(|url| url.trim_end_matches('/').to_string())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let credential = if let Some(token) = read_env("BPM_MCP_TOKEN") {
            Credential::Bearer(token)
        } else if let Some(key) = read_env("BPM_MCP_API_KEY") {
            Credential::ApiKey(key)
        } else {
            Credential::None
        };

        let rest_timeout = match read_env("BPM_MCP_TIMEOUT_MS").map(|v| v.parse::<u64>()) {
            Some(Ok(ms)) => Duration::from_millis(ms),
            Some(Err(e)) => {
                eprintln!(
                    "bpm-engine-mcp-server: ignoring malformed BPM_MCP_TIMEOUT_MS ({e}); \
                     using the default of {} ms",
                    DEFAULT_REST_TIMEOUT.as_millis()
                );
                DEFAULT_REST_TIMEOUT
            }
            None => DEFAULT_REST_TIMEOUT,
        };

        let config = Self {
            base_url,
            credential,
            rest_timeout,
        };
        config.check_cleartext_credential()?;
        Ok(config)
    }

    /// Fail closed when a credential would cross the network in
    /// cleartext: plaintext `http://` to a NON-loopback host while a
    /// token/API key is configured. `https://` and loopback plaintext
    /// (`localhost`, `127.0.0.1`, `::1`, and the rest of the loopback
    /// ranges) are always fine; open mode (no credential) has no secret
    /// to leak; the explicit `BPM_MCP_ALLOW_INSECURE_HTTP=1` opt-in
    /// admits the combination with a single warning on stderr.
    fn check_cleartext_credential(&self) -> anyhow::Result<()> {
        if self.credential == Credential::None {
            return Ok(()); // open mode: no secret can leak
        }
        // An unparseable base URL cannot be verified to be loopback or
        // https — refuse rather than guess (fail closed).
        let url = reqwest::Url::parse(&self.base_url).map_err(|e| {
            anyhow::anyhow!(
                "BPM_BASE_URL {:?} does not parse as a URL ({e}); refusing to send a \
                 credential to an unverifiable address — use a full http(s):// URL",
                self.base_url
            )
        })?;
        let host = url.host_str().unwrap_or_default();
        if url.scheme() != "http" || is_loopback(host) {
            return Ok(()); // not plaintext, or plaintext to loopback
        }
        if read_env(ALLOW_INSECURE_HTTP_ENV).as_deref() == Some("1") {
            eprintln!(
                "bpm-engine-mcp-server: WARNING: BPM_MCP_ALLOW_INSECURE_HTTP=1 — the \
                 configured credential will be sent in cleartext over http to {host}"
            );
            return Ok(());
        }
        anyhow::bail!(
            "refusing to start: the configured credential ({}) would be sent in \
             cleartext to non-loopback http://{host} — use an https:// base URL (or a \
             loopback host), or set BPM_MCP_ALLOW_INSECURE_HTTP=1 to accept the risk \
             explicitly",
            credential_var_name(&self.credential),
        )
    }
}

/// Whether a URL host is loopback: plaintext http to one of these cannot
/// carry the credential off the machine. `localhost` by name, or any
/// loopback IP (`127.0.0.1`, `::1`, and the rest of the ranges —
/// `IpAddr::is_loopback`). IPv6 hosts arrive from `Url::host_str` in
/// brackets; they are stripped first.
fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The environment variable a credential came from (error text only —
/// never the secret itself).
fn credential_var_name(credential: &Credential) -> &'static str {
    match credential {
        Credential::Bearer(_) => "BPM_MCP_TOKEN",
        Credential::ApiKey(_) => "BPM_MCP_API_KEY",
        Credential::None => "no credential",
    }
}

/// Read a non-empty env var (trimmed), `None` when unset/blank.
fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_loopback_and_open() {
        let config = McpConfig::default();
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.credential, Credential::None);
        assert_eq!(config.rest_timeout, DEFAULT_REST_TIMEOUT);
    }

    #[test]
    fn cleartext_guard_logic_over_parsed_urls() {
        // The guard's host/scheme classification, checked without env
        // mutation (the env-facing behavior is pinned in
        // tests/env_config.rs, which owns the environment).
        for (url, loopback) in [
            ("http://127.0.0.1:3000", true),
            ("http://localhost:3000", true),
            ("http://[::1]:3000", true),
            ("http://engine.internal:8080", false),
            ("http://10.0.0.5:3000", false),
        ] {
            let parsed = reqwest::Url::parse(url).expect("parses");
            assert_eq!(
                is_loopback(parsed.host_str().unwrap_or_default()),
                loopback,
                "{url}"
            );
            assert_eq!(parsed.scheme(), "http", "{url}");
        }
        let parsed = reqwest::Url::parse("https://engine.internal:8443").expect("parses");
        assert_eq!(parsed.scheme(), "https");
    }
}
