//! Environment-configuration tests (own test binary so env-var mutation
//! cannot race with other suites).
//!
//! The tests run serialized behind a process-wide lock (cargo runs test
//! functions in parallel by default, and these mutate the environment).

use bpm_engine_mcp_server::config::{
    Credential, McpConfig, DEFAULT_BASE_URL, DEFAULT_REST_TIMEOUT,
};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

/// Serializes environment mutation across this binary.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

const VARS: &[&str] = &[
    "BPM_BASE_URL",
    "BPM_MCP_TOKEN",
    "BPM_MCP_API_KEY",
    "BPM_MCP_TIMEOUT_MS",
    "BPM_MCP_ROLE",
    "BPM_MCP_ALLOWED_TOOLS",
    "BPM_MCP_ALLOW_INSECURE_HTTP",
];

fn clear_vars() {
    for var in VARS {
        std::env::remove_var(var);
    }
}

#[test]
fn defaults_without_any_env() {
    let _guard = env_lock();
    clear_vars();
    let config = McpConfig::from_env().expect("defaults build");
    assert_eq!(config.base_url, DEFAULT_BASE_URL);
    assert_eq!(config.credential, Credential::None);
    assert_eq!(config.rest_timeout, DEFAULT_REST_TIMEOUT);
}

#[test]
fn token_takes_precedence_over_api_key() {
    let _guard = env_lock();
    clear_vars();
    std::env::set_var("BPM_MCP_TOKEN", "jwt-abc");
    std::env::set_var("BPM_MCP_API_KEY", "key-xyz");
    let config = McpConfig::from_env().expect("config builds");
    assert_eq!(config.credential, Credential::Bearer("jwt-abc".to_string()));

    // Without the token, the API key is the credential.
    std::env::remove_var("BPM_MCP_TOKEN");
    let config = McpConfig::from_env().expect("config builds");
    assert_eq!(config.credential, Credential::ApiKey("key-xyz".to_string()));
}

#[test]
fn base_url_overrides_and_trailing_slash_is_trimmed() {
    let _guard = env_lock();
    clear_vars();
    std::env::set_var("BPM_BASE_URL", "http://engine.internal:8080/");
    // No credential: open mode has no secret to leak, so a remote
    // plaintext URL stays allowed.
    let config = McpConfig::from_env().expect("open mode allows remote http");
    assert_eq!(config.base_url, "http://engine.internal:8080");
    // Blank values fall back to the default.
    std::env::set_var("BPM_BASE_URL", "   ");
    let config = McpConfig::from_env().expect("config builds");
    assert_eq!(config.base_url, DEFAULT_BASE_URL);
}

#[test]
fn timeout_parsing_with_bad_value_falls_back() {
    let _guard = env_lock();
    clear_vars();
    std::env::set_var("BPM_MCP_TIMEOUT_MS", "2500");
    let config = McpConfig::from_env().expect("config builds");
    assert_eq!(config.rest_timeout, Duration::from_millis(2500));
    std::env::set_var("BPM_MCP_TIMEOUT_MS", "soon");
    let config = McpConfig::from_env().expect("config builds");
    assert_eq!(config.rest_timeout, DEFAULT_REST_TIMEOUT);
}

#[test]
fn cleartext_credential_to_non_loopback_http_fails_closed() {
    let _guard = env_lock();
    clear_vars();

    // http + token + remote → REFUSED, with the remedies in the message.
    std::env::set_var("BPM_MCP_TOKEN", "jwt-abc");
    std::env::set_var("BPM_BASE_URL", "http://engine.internal:8080");
    let err = McpConfig::from_env()
        .expect_err("a credential over plaintext non-loopback http must refuse to start");
    let message = err.to_string();
    assert!(message.contains("cleartext"), "{message}");
    assert!(message.contains("https"), "{message}");
    assert!(message.contains("BPM_MCP_ALLOW_INSECURE_HTTP"), "{message}");

    // http + API key + remote → refused identically (both credential
    // kinds carry the secret).
    std::env::remove_var("BPM_MCP_TOKEN");
    std::env::set_var("BPM_MCP_API_KEY", "key-xyz");
    let err = McpConfig::from_env().expect_err("the API key is a credential too");
    assert!(err.to_string().contains("BPM_MCP_API_KEY"), "{err}");

    // https + token + remote → fine.
    std::env::remove_var("BPM_MCP_API_KEY");
    std::env::set_var("BPM_MCP_TOKEN", "jwt-abc");
    std::env::set_var("BPM_BASE_URL", "https://engine.internal:8443");
    let config = McpConfig::from_env().expect("https to a remote host is fine");
    assert_eq!(config.base_url, "https://engine.internal:8443");

    // http + token + LOOPBACK → fine (every spelling).
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        std::env::set_var("BPM_BASE_URL", format!("http://{host}:3000"));
        let config = McpConfig::from_env().expect("plaintext loopback is fine");
        assert_eq!(
            config.base_url,
            format!("http://{host}:3000"),
            "loopback {host} must not be refused"
        );
    }

    // No credential + http + remote → fine (open mode: no secret to leak).
    std::env::remove_var("BPM_MCP_TOKEN");
    std::env::set_var("BPM_BASE_URL", "http://engine.internal:8080");
    McpConfig::from_env().expect("open mode has no secret to guard");

    // The explicit override admits the combination — and only the exact
    // value `1` does.
    std::env::set_var("BPM_MCP_TOKEN", "jwt-abc");
    std::env::set_var("BPM_MCP_ALLOW_INSECURE_HTTP", "1");
    McpConfig::from_env().expect("the explicit opt-in admits it");
    std::env::set_var("BPM_MCP_ALLOW_INSECURE_HTTP", "yes");
    McpConfig::from_env().expect_err("only the exact value 1 overrides");
}

#[test]
fn allowlist_resolves_from_env_end_to_end() {
    let _guard = env_lock();
    clear_vars();
    // Default: the read-only surface.
    let tools = bpm_engine_mcp_server::allowlist::resolve_from_env().expect("resolves");
    assert_eq!(
        tools,
        bpm_engine_mcp_server::allowlist::DEFAULT_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
    // Role preset from env.
    std::env::set_var("BPM_MCP_ROLE", "worker");
    let tools = bpm_engine_mcp_server::allowlist::resolve_from_env().expect("resolves");
    assert!(tools.contains(&"fetch_and_lock_external_tasks".to_string()));
    assert!(!tools.contains(&"start_process_instance".to_string()));
    // Down-scope via the explicit list.
    std::env::set_var("BPM_MCP_ALLOWED_TOOLS", "list_process_instances");
    let tools = bpm_engine_mcp_server::allowlist::resolve_from_env().expect("resolves");
    assert_eq!(tools, vec!["list_process_instances".to_string()]);
    // A typo fails closed with the valid names.
    std::env::set_var("BPM_MCP_ALLOWED_TOOLS", "list_proces_instances");
    let err = bpm_engine_mcp_server::allowlist::resolve_from_env().expect_err("typo fails");
    assert!(err.contains("v1 tool surface"), "{err}");
}
