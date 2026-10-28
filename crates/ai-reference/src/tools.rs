//! The reference suite's side-effectful tool (E3-3 demo).
//!
//! [`LedgerPostTool`] is what the 工具 (tool) and 审批 (approval) flows
//! execute: a tool that "posts an entry to an external ledger". It is
//! deliberately the full E3-3 exercise:
//!
//! - **Side-effectful** ([`ToolSpec::side_effects`]) → the approval gate
//!   applies: without the `__approval` task input the executor refuses with
//!   `tool_requires_approval` (fail-closed, issue #17's
//!   审批前禁止执行有副作用工具).
//! - **Credential from the worker environment only** — the key is read by
//!   the [`CredentialInjector`](bpm_engine_worker_sdk::CredentialInjector)
//!   from [`reference_credential_source`]'s env var, never from task
//!   variables.
//! - **Masked output** — the tool embeds the credential in its raw output
//!   (so the masking pipeline is observable); `TaskOutput::add_tool_output`
//!   replaces it with `***` before anything is persisted.
//!
//! The tool itself is pure bookkeeping (a formatted string): real workers
//! would put their HTTP/file/code side effects here — idempotently, since
//! execution is at-least-once (`docs/recovery.md` §8).

use std::sync::Arc;

use async_trait::async_trait;
use bpm_engine_worker_sdk::tools::{
    CredentialSource, Tool, ToolContext, ToolError, ToolExecutor, ToolSpec,
};

/// The reference ledger tool's name (its allowlist key).
pub const LEDGER_TOOL_NAME: &str = "ledger_post";

/// The reference ledger tool's version.
pub const LEDGER_TOOL_VERSION: &str = "1.0";

/// The key tool implementations read the credential under.
pub const LEDGER_CREDENTIAL_KEY: &str = "ledger_api_key";

/// The reference side-effectful tool: posts `input` to "the ledger" and
/// returns a raw output that embeds the credential (so the E3-3 output
/// pipeline's masking is observable in flow outcomes).
#[derive(Debug, Clone, Copy, Default)]
pub struct LedgerPostTool;

#[async_trait]
impl Tool for LedgerPostTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(LEDGER_TOOL_NAME, LEDGER_TOOL_VERSION).side_effects()
    }

    async fn run(&self, input: &str, ctx: &ToolContext) -> Result<String, ToolError> {
        // Real tool: an idempotent HTTP POST to the ledger service. The
        // reference formats the posting and embeds the credential so the
        // boundary's masking is observable downstream.
        let key = ctx
            .credentials
            .get(LEDGER_CREDENTIAL_KEY)
            .cloned()
            .unwrap_or_default();
        Ok(format!("ledger: posted '{input}' with key={key}"))
    }
}

/// The reference tool allowlist: one executor, one tool
/// ([`LedgerPostTool`]). Deny by default stays in force for everything
/// else.
pub fn reference_executor() -> ToolExecutor {
    let mut executor = ToolExecutor::new();
    executor
        .register_tool(Arc::new(LedgerPostTool))
        .expect("first registration of the reference tool");
    executor
}

/// The reference credential source: env var `env_var` (default
/// `BPM_REFERENCE_LEDGER_KEY`, overridable via
/// [`FlowOptions::tool_env_var`](crate::FlowOptions::tool_env_var)),
/// injected as `ledger_api_key` for tool implementations.
///
/// The env var is read when the injector resolves (per task), never from
/// task variables — see `docs/tool-security.md`.
pub fn reference_credential_source(env_var: &str) -> CredentialSource {
    CredentialSource::env(env_var).inject_as(LEDGER_CREDENTIAL_KEY)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ledger_tool_is_side_effectful_and_gated() {
        let executor = reference_executor();
        let spec = executor
            .registry()
            .spec(LEDGER_TOOL_NAME)
            .expect("ledger tool registered");
        assert!(spec.has_side_effects);
        assert!(spec.requires_approval);
        assert_eq!(spec.version, LEDGER_TOOL_VERSION);

        // Unapproved: the gate refuses (fail-closed).
        let ctx = ToolContext::new("task-1", "worker-1");
        let err = executor
            .execute(LEDGER_TOOL_NAME, "entry", &ctx)
            .await
            .expect_err("side-effect tool without approval must be refused");
        assert_eq!(err.code(), "tool_requires_approval");

        // Approved: executes and embeds the credential in the RAW output.
        let mut credentials = std::collections::HashMap::new();
        credentials.insert(
            LEDGER_CREDENTIAL_KEY.to_string(),
            "sk-ledger-test".to_string(),
        );
        let approved = ctx.credentials(credentials).approved(true);
        let raw = executor
            .execute(LEDGER_TOOL_NAME, "entry-1", &approved)
            .await
            .expect("approved tool executes");
        assert_eq!(raw, "ledger: posted 'entry-1' with key=sk-ledger-test");
    }

    #[tokio::test]
    async fn unknown_tools_are_denied_by_default() {
        let executor = reference_executor();
        let ctx = ToolContext::new("task-1", "worker-1").approved(true);
        let err = executor
            .execute("shell", "rm -rf /", &ctx)
            .await
            .expect_err("unregistered tool must be denied");
        assert_eq!(err.code(), "tool_not_allowed");
    }

    #[test]
    fn credential_source_targets_ledger_key() {
        let source = reference_credential_source("MY_LEDGER_ENV");
        assert_eq!(
            source,
            CredentialSource::env("MY_LEDGER_ENV").inject_as(LEDGER_CREDENTIAL_KEY)
        );
    }
}
