//! Worker-side tool security boundary (E3-3).
//!
//! The engine never dereferences artifact URIs and never runs tools: all side
//! effects of an AI-driven external task happen **inside worker processes**
//! (at-least-once — `docs/recovery.md` §8). That makes the worker SDK the
//! only place a security boundary for tool calls can exist. This module is
//! that boundary; it deliberately ships **no tool implementations** — workers
//! bring their own ([`Tool`]) and get four enforced properties:
//!
//! 1. **Allowlist, deny by default** ([`ToolRegistry`] / [`ToolExecutor`]):
//!    an empty executor denies every tool; only explicitly registered tools
//!    execute, otherwise the typed error `tool_not_allowed`.
//! 2. **Approval gate** (审批前禁止副作用工具): a tool declared with side
//!    effects — or explicitly marked approval-requiring — never executes
//!    unless the task carries approval ([`ToolContext::approved`], derived
//!    from the [`APPROVAL_VARIABLE`] task input). Otherwise
//!    `tool_requires_approval`.
//! 3. **Credential injection, process environment only** (凭证注入):
//!    [`CredentialInjector`] reads secrets from worker env vars, never from
//!    task variables (which are persisted in stores/history and delivered
//!    over the wire — see `docs/variables-and-artifacts.md`).
//! 4. **Output limits + sensitive redaction** (输出大小限制 / 敏感信息脱敏):
//!    [`apply_output_limit`] produces the engine's exact `__truncated:`
//!    markers (parity with the E2-3 history contract by delegating to core),
//!    and [`redact_variables`] / [`redact_string`] / [`mask_credentials_in`]
//!    mask secrets before they are persisted or logged.
//!
//! Full contract: `docs/tool-security.md`.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//!
//! use async_trait::async_trait;
//! use bpm_engine_worker_sdk::tools::{Tool, ToolContext, ToolError, ToolExecutor, ToolSpec};
//!
//! struct WebSearch;
//!
//! #[async_trait]
//! impl Tool for WebSearch {
//!     fn spec(&self) -> ToolSpec {
//!         ToolSpec::new("web_search", "2.1")
//!     }
//!     async fn run(&self, input: &str, ctx: &ToolContext) -> Result<String, ToolError> {
//!         // Read secrets from ctx.credentials (injected from env), never from task variables.
//!         let _key = ctx.credentials.get("search_api_key").map(String::as_str);
//!         Ok(format!("results for {input}"))
//!     }
//! }
//!
//! fn main() {
//!     let mut executor = ToolExecutor::new();
//!     executor
//!         .register_tool(Arc::new(WebSearch))
//!         .expect("first registration");
//!
//!     let rt = tokio::runtime::Runtime::new().expect("worker runtime");
//!     let ctx = ToolContext::new("task-1", "worker-1");
//!     // Registered, no side effects, no approval needed: executes.
//!     let output = rt.block_on(executor.execute("web_search", "bpm", &ctx));
//!     assert_eq!(output.unwrap(), "results for bpm");
//!     // Not registered: denied (deny by default).
//!     let err = rt
//!         .block_on(executor.execute("shell", "rm -rf /", &ctx))
//!         .unwrap_err();
//!     assert_eq!(err.code(), "tool_not_allowed");
//! }
//! ```

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bpm_engine_core::variable_contract::{
    redact_history_variable_value, HistoryRedactionPolicy, TRUNCATED_PREFIX,
};
use bpm_engine_core::ToolRef;
use thiserror::Error;
use tracing::debug;

use crate::ai_task::TaskOutput;
use crate::types::TaskResult;

/// Task input variable carrying this task's approval for side-effect tools
/// (plain `"true"` / `"false"`, absent → not approved).
///
/// v1 contract: a human approver sets it through task variables; the
/// engine-side persistent approval flow (E5-2) will set it automatically in
/// a future release. Workers derive [`ToolContext::approved`] from it via
/// [`TaskInput::approval_granted`](crate::ai_task::TaskInput::approval_granted).
pub const APPROVAL_VARIABLE: &str = "__approval";

/// Default per-value byte cap applied to tool outputs (64 KiB).
///
/// Matches the engine's default `variable_max_bytes` cap (E2-3, enforced at
/// the REST write boundary with 4xx `variable_too_large`): a larger value
/// would be rejected server-side anyway, so the worker truncates BEFORE
/// submitting — keeping the payload inspectable via the truncation marker
/// instead of losing the whole completion. If the server runs a smaller cap,
/// configure [`OutputLimits`] to match.
pub const DEFAULT_TOOL_OUTPUT_MAX_BYTES: usize = 65_536;

/// Mask replacing redacted values and credential occurrences (`"***"`).
pub const REDACTION_MASK: &str = "***";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Typed errors from the tool security boundary.
///
/// Every variant carries a stable machine-readable [`code`](Self::code) in
/// the snake_case convention shared with the E2-3/E3-1 error contracts. The
/// four execution-boundary codes (`tool_not_allowed`,
/// `tool_requires_approval`, `credential_missing`, `tool_execution_failed`)
/// map to [`TaskResult::Fail`] via the [`From`] impl, with the code leading
/// the error message (`"<code>: <message>"`, the same shape as REST error
/// bodies) so it stays readable in the engine's `error_message`, the
/// `ExternalTaskFailed` history payload and the DLQ.
///
/// Retry policy: the [`From`] impl uses `retry_after: None` (engine default
/// schedule). Notably `tool_not_allowed` / `tool_requires_approval` /
/// `credential_missing` are permanent until the task inputs or the worker's
/// environment change — acting on that distinction is E3-2 scope.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolError {
    /// The tool is not registered in this worker's allowlist (deny by
    /// default).
    #[error("tool '{tool}' is not in this worker's tool allowlist (deny by default); only tools registered via ToolExecutor::register_tool may execute")]
    ToolNotAllowed {
        /// The denied tool name, exactly as requested.
        tool: String,
    },
    /// The tool has side effects (or is explicitly approval-requiring) and
    /// the task carries no approval.
    #[error("tool '{tool}' must not execute before approval: it has side effects and this task is not approved. Grant approval via the task input variable {APPROVAL_VARIABLE} = \"true\" (set through task variables today; the engine-side persistent approval flow, E5-2, will set it automatically in a future release)")]
    ToolRequiresApproval {
        /// The blocked tool name.
        tool: String,
    },
    /// A credential source is missing from the worker process environment.
    /// The error names the env var, never the value.
    #[error("credential env var '{env_var}' is not set in the worker process (or is not valid UTF-8); credentials are read from the worker environment only, never from task variables")]
    CredentialMissing {
        /// The missing env var name (the value is never included).
        env_var: String,
    },
    /// A registered, approved tool failed while running.
    #[error("tool '{tool}' failed: {reason}")]
    ToolExecutionFailed {
        /// The tool that failed.
        tool: String,
        /// Failure reason from the tool implementation (must not carry
        /// credential values — mask with
        /// [`mask_credentials_in`]
        /// before embedding).
        reason: String,
    },
    /// A duplicate registration was rejected (registration-time code, not an
    /// execution-boundary code). Nothing changed: the first registration
    /// wins.
    #[error("tool '{tool}' is already registered; duplicate registration is rejected and the existing entry is kept")]
    ToolAlreadyRegistered {
        /// The tool name that was already registered.
        tool: String,
    },
    /// A registration with an empty (or whitespace-only) tool name was
    /// rejected. Tool names must be non-empty — an empty name would also be
    /// rejected downstream by the envelope's tool validation.
    #[error("tool name must not be empty or whitespace-only")]
    InvalidToolName,
}

impl ToolError {
    /// Stable machine-readable snake_case error code.
    pub fn code(&self) -> &'static str {
        match self {
            ToolError::ToolNotAllowed { .. } => "tool_not_allowed",
            ToolError::ToolRequiresApproval { .. } => "tool_requires_approval",
            ToolError::CredentialMissing { .. } => "credential_missing",
            ToolError::ToolExecutionFailed { .. } => "tool_execution_failed",
            ToolError::ToolAlreadyRegistered { .. } => "tool_already_registered",
            ToolError::InvalidToolName => "invalid_tool_name",
        }
    }
}

impl From<ToolError> for TaskResult {
    /// Map a tool error to a task failure whose message carries the code
    /// (`"<code>: <message>"`) — readable in the engine's error message,
    /// history and DLQ. `retry_after` is `None` (engine default schedule).
    fn from(err: ToolError) -> Self {
        TaskResult::Fail {
            error: format!("{}: {}", err.code(), err),
            retry_after: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Allowlist: ToolSpec + ToolRegistry
// ---------------------------------------------------------------------------

/// One allowlisted tool: identity plus the policy flags the boundary
/// enforces.
///
/// - `has_side_effects` — the tool changes the world (HTTP POST, payment,
///   file write, code exec). **Always** implies the approval gate: the SDK
///   refuses to run a side-effectful tool before approval even if
///   `requires_approval` was left `false` (fail-closed — the acceptance rule
///   of issue #17 is "审批前禁止执行有副作用工具").
/// - `requires_approval` — additionally forces the approval gate for
///   side-effect-free tools (e.g. an expensive or sensitive read-only call).
///
/// Side-effectful tools must also be **idempotent**: execution is
/// at-least-once (redelivery after lease expiry or worker crash —
/// `docs/recovery.md` §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    /// Tool name (non-empty; the key the executor dispatches on).
    pub name: String,
    /// Tool version (non-empty; recorded in the AI task envelope when the
    /// tool runs).
    pub version: String,
    /// The tool changes external state. Implies the approval gate.
    pub has_side_effects: bool,
    /// The tool requires approval even without side effects.
    pub requires_approval: bool,
}

impl ToolSpec {
    /// A side-effect-free, approval-free tool spec.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            has_side_effects: false,
            requires_approval: false,
        }
    }

    /// Mark the tool as side-effectful — which also turns on the approval
    /// gate (see [`ToolSpec`]); use [`requires_approval`](Self::requires_approval)
    /// alone for approval-gated read-only tools.
    pub fn side_effects(mut self) -> Self {
        self.has_side_effects = true;
        self.requires_approval = true;
        self
    }

    /// Force the approval gate even for a side-effect-free tool.
    pub fn requires_approval(mut self) -> Self {
        self.requires_approval = true;
        self
    }
}

impl From<&ToolSpec> for ToolRef {
    /// Project a spec into the E3-1 envelope's tool reference (empty
    /// `version` becomes `None` to stay valid under
    /// `AiTaskEnvelope::validate`).
    fn from(spec: &ToolSpec) -> Self {
        ToolRef {
            name: spec.name.clone(),
            version: if spec.version.is_empty() {
                None
            } else {
                Some(spec.version.clone())
            },
        }
    }
}

/// The allowlist: tool specs by name. Deny by default — an empty registry
/// allows nothing.
///
/// Standalone use is for **observation** (cheap `is_allowed` pre-checks,
/// building the tool list a model is told about). **Enforcement** lives in
/// [`ToolExecutor`], whose internal registry is populated exclusively by
/// [`ToolExecutor::register_tool`] so the enforced allowlist can never
/// diverge from the set of runnable implementations.
///
/// Duplicate registration is rejected with
/// [`ToolError::ToolAlreadyRegistered`] and the registry is left unchanged
/// (first registration wins; no silent replacement, no panic).
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    specs: HashMap<String, ToolSpec>,
}

impl ToolRegistry {
    /// An empty registry — allows nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one spec. A duplicate name or an empty name is rejected
    /// (`tool_already_registered` / `invalid_tool_name`) and changes nothing.
    pub fn register(&mut self, spec: ToolSpec) -> Result<&mut Self, ToolError> {
        if spec.name.trim().is_empty() {
            return Err(ToolError::InvalidToolName);
        }
        if self.specs.contains_key(&spec.name) {
            return Err(ToolError::ToolAlreadyRegistered { tool: spec.name });
        }
        self.specs.insert(spec.name.clone(), spec);
        Ok(self)
    }

    /// Whether `name` is allowlisted.
    pub fn is_allowed(&self, name: &str) -> bool {
        self.specs.contains_key(name)
    }

    /// The spec registered under `name`, if any.
    pub fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.specs.get(name)
    }

    /// Number of allowlisted tools.
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Whether no tool is allowlisted (everything is denied).
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// All allowlisted tool names (unordered) — e.g. the tool list a worker
    /// advertises to a model.
    pub fn names(&self) -> Vec<&str> {
        self.specs.keys().map(String::as_str).collect()
    }
}

// ---------------------------------------------------------------------------
// Execution context
// ---------------------------------------------------------------------------

/// Per-invocation context handed to [`Tool`] implementations and consumed
/// by the output pipeline ([`TaskOutput::add_tool_output`]).
///
/// Build one per task: `ToolContext::new(task_id, worker_id).approved(input
/// .approval_granted()).credentials(injector.resolve()?)` — see
/// [`APPROVAL_VARIABLE`] for the approval input contract.
#[derive(Clone)]
pub struct ToolContext {
    /// The external task being handled (for idempotency keys and logging).
    pub task_id: String,
    /// The worker executing the task.
    pub worker_id: String,
    /// Whether this task carries approval for side-effect tools — derive
    /// from [`TaskInput::approval_granted`](crate::ai_task::TaskInput::approval_granted),
    /// do not guess.
    pub approved: bool,
    /// Secrets injected from the worker environment
    /// ([`CredentialInjector::resolve`]). What tool implementations read;
    /// mask any occurrence in outputs with
    /// [`mask_credentials_in`].
    pub credentials: HashMap<String, String>,
    /// Byte cap applied to tool output variables ([`apply_output_limit`]).
    pub output_limits: OutputLimits,
    /// Name-based output redaction policy ([`redact_variables`]).
    pub redaction: RedactionPolicy,
}

/// Manual `Debug`: credential VALUES are never printed — only their keys.
/// A derived impl would leak every secret through `{:?}` / `tracing`.
impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("task_id", &self.task_id)
            .field("worker_id", &self.worker_id)
            .field("approved", &self.approved)
            .field(
                "credentials",
                &self
                    .credentials
                    .keys()
                    .collect::<std::collections::BTreeSet<_>>(),
            )
            .field("output_limits", &self.output_limits)
            .field("redaction", &self.redaction)
            .finish()
    }
}

impl ToolContext {
    /// An unapproved context with no credentials, default output limits
    /// (64 KiB) and default redaction (no patterns, `"***"` mask).
    pub fn new(task_id: impl Into<String>, worker_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            worker_id: worker_id.into(),
            approved: false,
            credentials: HashMap::new(),
            output_limits: OutputLimits::default(),
            redaction: RedactionPolicy::default(),
        }
    }

    /// Set whether this task is approved.
    pub fn approved(mut self, approved: bool) -> Self {
        self.approved = approved;
        self
    }

    /// Set the injected credentials.
    pub fn credentials(mut self, credentials: HashMap<String, String>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Set the output limits.
    pub fn output_limits(mut self, limits: OutputLimits) -> Self {
        self.output_limits = limits;
        self
    }

    /// Set the redaction policy.
    pub fn redaction(mut self, policy: RedactionPolicy) -> Self {
        self.redaction = policy;
        self
    }
}

// ---------------------------------------------------------------------------
// Allowlist enforcement: Tool + ToolExecutor
// ---------------------------------------------------------------------------

/// One tool implementation owned by the worker process. The SDK provides the
/// security boundary, not the tools — anything an AI worker invokes (HTTP
/// calls, code, file access) is implemented here, by the worker author.
///
/// [`run`](Tool::run) is only invoked after the executor's allowlist and
/// approval checks passed. Implementations:
/// - read secrets from `ctx.credentials`, never from task variables;
/// - must be idempotent when `spec().has_side_effects` (at-least-once
///   execution, `docs/recovery.md` §8);
/// - return raw output — masking and size limits are applied by the worker
///   via [`TaskOutput::add_tool_output`] / [`apply_output_limit`] /
///   [`mask_credentials_in`] before persisting or logging.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The allowlist spec declaring this tool's identity and policy flags.
    fn spec(&self) -> ToolSpec;

    /// Execute the tool for one call.
    async fn run(&self, input: &str, ctx: &ToolContext) -> Result<String, ToolError>;
}

/// Allowlist-enforcing tool dispatcher: the worker-side security boundary.
///
/// Deny by default: a fresh [`ToolExecutor`] has an empty allowlist and
/// denies every tool with `tool_not_allowed`. [`register_tool`] is the ONLY
/// way a tool enters the allowlist — the implementation and its [`ToolSpec`]
/// register together, so what is allowed to run and what can run are always
/// the same set.
///
/// [`execute`] enforces, in order:
/// 1. allowlist: unregistered name → [`ToolError::ToolNotAllowed`];
/// 2. approval gate: `requires_approval || has_side_effects` while
///    `!ctx.approved` → [`ToolError::ToolRequiresApproval`];
/// 3. dispatch to the implementation (errors surfaced as
///    [`ToolError::ToolExecutionFailed`] by the implementation itself).
///
/// [`register_tool`]: ToolExecutor::register_tool
/// [`execute`]: ToolExecutor::execute
#[derive(Default)]
pub struct ToolExecutor {
    registry: ToolRegistry,
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl std::fmt::Debug for ToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Tool implementations are not required to be Debug; the allowlist
        // (same key set as the implementations) fully describes the state.
        f.debug_struct("ToolExecutor")
            .field("registry", &self.registry)
            .finish()
    }
}

impl ToolExecutor {
    /// An empty executor — denies every tool (deny by default).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one tool implementation; its spec joins the allowlist.
    /// A duplicate name is rejected (`tool_already_registered`) and changes
    /// nothing — the first registration wins.
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) -> Result<&mut Self, ToolError> {
        let spec = tool.spec();
        let name = spec.name.clone();
        if self.tools.contains_key(&name) {
            return Err(ToolError::ToolAlreadyRegistered { tool: name });
        }
        // The registry and the implementation map hold the same key set, so
        // this cannot fail here; `?` keeps the two structures in lockstep.
        self.registry.register(spec)?;
        self.tools.insert(name, tool);
        Ok(self)
    }

    /// The enforced allowlist (specs of the registered tools).
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Execute one tool call through the boundary.
    ///
    /// Returns the tool's raw output (mask and limit it before persisting or
    /// logging — see [`TaskOutput::add_tool_output`]).
    pub async fn execute(
        &self,
        name: &str,
        input: &str,
        ctx: &ToolContext,
    ) -> Result<String, ToolError> {
        // 1. Allowlist (deny by default).
        let Some(spec) = self.registry.spec(name) else {
            return Err(ToolError::ToolNotAllowed {
                tool: name.to_string(),
            });
        };
        // 2. Approval gate: side effects always require approval, and
        //    `requires_approval` additionally gates side-effect-free tools.
        if (spec.requires_approval || spec.has_side_effects) && !ctx.approved {
            return Err(ToolError::ToolRequiresApproval {
                tool: spec.name.clone(),
            });
        }
        // 3. Dispatch. (Unreachable while register_tool is the only way in;
        //    fail closed regardless.)
        let Some(tool) = self.tools.get(name) else {
            return Err(ToolError::ToolExecutionFailed {
                tool: name.to_string(),
                reason: "allowlisted but no implementation registered (worker misconfiguration)"
                    .to_string(),
            });
        };
        tool.run(input, ctx).await
    }
}

// ---------------------------------------------------------------------------
// Credential injection (process environment only)
// ---------------------------------------------------------------------------

/// One credential source. v1 has exactly one variant: the worker process
/// environment.
///
/// Credentials are NEVER sourced from task variables: task variables are
/// persisted (instance state, external task rows, event history, the DLQ)
/// and delivered over the wire, so a secret placed there would leak into
/// storage and logs — the environment stays inside the worker process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// Read the credential from the worker process environment variable
    /// `var_name` and expose it to tools as `credentials[inject_as]`.
    Env {
        /// The environment variable to read.
        var_name: String,
        /// The key tool implementations read the value under.
        inject_as: String,
    },
}

impl CredentialSource {
    /// An env source whose `inject_as` key defaults to the env var name.
    pub fn env(var_name: impl Into<String>) -> Self {
        let var_name = var_name.into();
        let inject_as = var_name.clone();
        CredentialSource::Env {
            var_name,
            inject_as,
        }
    }

    /// Override the key tool implementations read (`inject_as`).
    pub fn inject_as(mut self, inject_as: impl Into<String>) -> Self {
        // Irrefutable while `Env` is the only variant; a future variant
        // turns this into a compile error that must be handled.
        let CredentialSource::Env { inject_as: key, .. } = &mut self;
        *key = inject_as.into();
        self
    }
}

/// Resolves [`CredentialSource`]s into the credential map for a
/// [`ToolContext`].
///
/// Resolve at worker startup where possible: a missing variable is a worker
/// deployment error you want to surface at boot, not per task. Values are
/// never logged by this type; [`ToolError::CredentialMissing`] names the env
/// var only.
#[derive(Debug, Clone, Default)]
pub struct CredentialInjector {
    sources: Vec<CredentialSource>,
}

impl CredentialInjector {
    /// An injector with no sources.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one source (builder style).
    pub fn source(mut self, source: CredentialSource) -> Self {
        self.sources.push(source);
        self
    }

    /// All configured sources.
    pub fn sources(&self) -> &[CredentialSource] {
        &self.sources
    }

    /// Resolve every source against the worker process environment.
    ///
    /// - Present env var → `credentials[inject_as] = value`.
    /// - Missing or non-UTF-8 env var → [`ToolError::CredentialMissing`]
    ///   naming the env var (never the value — there is none to leak).
    /// - Two sources with the same `inject_as` key: the later one wins.
    pub fn resolve(&self) -> Result<HashMap<String, String>, ToolError> {
        let mut credentials = HashMap::with_capacity(self.sources.len());
        for source in &self.sources {
            match source {
                CredentialSource::Env {
                    var_name,
                    inject_as,
                } => match std::env::var(var_name) {
                    Ok(value) => {
                        credentials.insert(inject_as.clone(), value);
                    }
                    Err(_) => {
                        return Err(ToolError::CredentialMissing {
                            env_var: var_name.clone(),
                        })
                    }
                },
            }
        }
        Ok(credentials)
    }
}

/// Replace every occurrence of every credential value in `value` with
/// `"***"` — for logs and tool outputs.
///
/// Empty credential values are skipped (they would match everywhere), and
/// longer values are masked before shorter ones so overlapping secrets
/// cannot survive as partial remnants. See also
/// [`redact_string`].
pub fn mask_credentials_in(value: &str, credentials: &HashMap<String, String>) -> String {
    let values: Vec<String> = credentials
        .values()
        .filter(|v| !v.is_empty())
        .cloned()
        .collect();
    redact_string(value, &values)
}

// ---------------------------------------------------------------------------
// Output size limits
// ---------------------------------------------------------------------------

/// Byte cap for tool output values.
///
/// The default (64 KiB) matches the engine's `variable_max_bytes` REST
/// boundary cap: a value above the server's cap is rejected with 4xx
/// `variable_too_large` after submission, so the worker truncates BEFORE
/// submitting and the completion still carries an inspectable marker.
/// Align `max_bytes` with the server's configured cap when it differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLimits {
    /// Maximum byte length of a tool output value kept verbatim.
    pub max_bytes: usize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_TOOL_OUTPUT_MAX_BYTES,
        }
    }
}

/// Apply the output limit to one tool output value.
///
/// Values at or below `limits.max_bytes` are returned verbatim. Longer
/// values become `__truncated:<orig_len>:<sha256-first-12-hex>:` markers —
/// byte-identical to the engine's history redaction markers, because this
/// delegates to the SAME core function
/// ([`redact_history_variable_value`]). A tool output that happens to be an
/// unknown envelope version (`__var:2:…`) is likewise mapped to `__opaque:`
/// exactly like the core contract. `name` is used for logging only.
pub fn apply_output_limit(name: &str, value: &str, limits: &OutputLimits) -> String {
    let policy = HistoryRedactionPolicy {
        max_variable_bytes: limits.max_bytes,
    };
    let limited = redact_history_variable_value(value, &policy);
    if limited != value {
        debug!(
            tool = %name,
            original_bytes = value.len(),
            max_bytes = limits.max_bytes,
            marker = %TRUNCATED_PREFIX,
            "tool output over the byte cap, replaced with a truncation marker"
        );
    }
    limited
}

// ---------------------------------------------------------------------------
// Sensitive redaction
// ---------------------------------------------------------------------------

/// Name-based redaction policy for output variables.
///
/// [`redact_variables`] masks the VALUE of any variable whose NAME matches a
/// pattern. Patterns are exact names or trailing-wildcard prefixes
/// (`"api_key"`, `"token_*"`); matching is case-sensitive. Substring masking
/// inside values is [`redact_string`]'s job.
///
/// No regex — a deliberate tradeoff: predictable, dependency-free, auditable
/// matching in exchange for a limited pattern vocabulary (exact + prefix
/// only). Piling on regex would widen the attack surface of a security
/// boundary for little gain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPolicy {
    /// Variable names whose values are masked: exact match, or prefix match
    /// when the pattern ends with `*` (e.g. `"token_*"`; a bare `"*"`
    /// redacts everything).
    pub redact_variable_names: Vec<String>,
    /// Replacement for redacted values (default `"***"`).
    pub mask: String,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            redact_variable_names: Vec::new(),
            mask: REDACTION_MASK.to_string(),
        }
    }
}

impl RedactionPolicy {
    /// A policy from name patterns with the default `"***"` mask.
    pub fn redact_variable_names(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.redact_variable_names = names.into_iter().map(Into::into).collect();
        self
    }
}

/// Whether `name` matches one of the redaction patterns (exact, or prefix
/// when the pattern ends with `*` — a `*` anywhere else is literal).
fn variable_name_matches(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        pattern
            .strip_suffix('*')
            .is_some_and(|prefix| name.starts_with(prefix))
            || pattern.as_str() == name
    })
}

/// Redact a variable map by NAME: every variable whose name matches the
/// policy gets its whole value replaced with `policy.mask`; other entries
/// pass through verbatim (value substrings are not touched — use
/// [`redact_string`] for that).
pub fn redact_variables(
    map: &HashMap<String, String>,
    policy: &RedactionPolicy,
) -> HashMap<String, String> {
    map.iter()
        .map(|(name, value)| {
            let redacted = if variable_name_matches(name, &policy.redact_variable_names) {
                policy.mask.clone()
            } else {
                value.clone()
            };
            (name.clone(), redacted)
        })
        .collect()
}

/// Mask literal substrings: every occurrence of each pattern in `value` is
/// replaced with `"***"`.
///
/// Longer patterns are replaced first (overlapping secrets cannot survive as
/// partial remnants); empty patterns are skipped. Typically called with
/// credential values (see [`mask_credentials_in`], which wraps this). No
/// regex — literal substring matching only.
pub fn redact_string(value: &str, patterns: &[String]) -> String {
    let mut ordered: Vec<&String> = patterns.iter().filter(|p| !p.is_empty()).collect();
    ordered.sort_by_key(|p| Reverse(p.len()));
    let mut masked = value.to_string();
    for pattern in ordered {
        if masked.contains(pattern.as_str()) {
            masked = masked.replace(pattern.as_str(), REDACTION_MASK);
        }
    }
    masked
}

// ---------------------------------------------------------------------------
// TaskOutput integration
// ---------------------------------------------------------------------------

impl TaskOutput {
    /// Add one TOOL output variable with the security pipeline applied:
    /// name-based redaction, then credential masking, then the output limit.
    ///
    /// Redaction runs BEFORE the size limit so the `__truncated:` marker's
    /// digest is computed over the REDACTED value — the marker must never
    /// fingerprint a secret-bearing string (an offline-verifiable digest of
    /// a secret-containing value would leak).
    ///
    /// Prefer this over [`variable`](TaskOutput::variable) for anything a
    /// tool produced.
    pub fn add_tool_output(self, name: &str, value: &str, ctx: &ToolContext) -> Self {
        // 1. Name-based redaction: a sensitive variable name masks the value.
        let redacted = if variable_name_matches(name, &ctx.redaction.redact_variable_names) {
            ctx.redaction.mask.clone()
        } else {
            value.to_string()
        };
        // 2. Credential masking: no secret value survives into the output.
        let redacted = mask_credentials_in(&redacted, &ctx.credentials);
        // 3. Output limit (with the core marker format).
        let limited = apply_output_limit(name, &redacted, &ctx.output_limits);
        self.variable(name, limited)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_task::TaskInput;
    use crate::handler::{TaskContext, TaskHandler};
    use crate::types::ExternalTask;
    use crate::EngineClient;

    /// SHA-256("hello") — the same well-known vector core's contract tests
    /// use, guarding marker parity against digest drift.
    const HELLO_MARKER: &str = "__truncated:5:2cf24dba5fb0:";

    #[test]
    fn tool_context_debug_never_prints_credential_values() {
        let mut credentials = HashMap::new();
        credentials.insert("API_KEY".to_string(), "super-secret-value".to_string());
        let ctx = ToolContext::new("task-1", "worker-1").credentials(credentials);
        let rendered = format!("{:?}", ctx);
        assert!(
            !rendered.contains("super-secret-value"),
            "Debug must never include credential values: {rendered}"
        );
        assert!(
            rendered.contains("API_KEY"),
            "Debug should still show credential keys for diagnostics: {rendered}"
        );
    }

    #[test]
    fn empty_tool_name_is_rejected_at_registration() {
        let mut registry = ToolRegistry::new();
        let err = registry.register(ToolSpec::new("", "1.0")).unwrap_err();
        assert_eq!(err.code(), "invalid_tool_name");
        assert!(registry.is_empty());
    }

    #[test]
    fn whitespace_tool_name_is_rejected_at_registration() {
        let mut registry = ToolRegistry::new();
        let err = registry.register(ToolSpec::new("   ", "1.0")).unwrap_err();
        assert_eq!(err.code(), "invalid_tool_name");
    }

    // --- Test doubles -------------------------------------------------------

    /// A tool that echoes its input and embeds the (fake) credential, so
    /// output masking is observable. Read-only by default.
    struct EchoTool {
        spec: ToolSpec,
        embed_credential: bool,
    }

    impl EchoTool {
        fn read_only(name: &str) -> Self {
            EchoTool {
                spec: ToolSpec::new(name, "1.0"),
                embed_credential: false,
            }
        }
    }

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            self.spec.clone()
        }

        async fn run(&self, input: &str, ctx: &ToolContext) -> Result<String, ToolError> {
            let secret = ctx
                .credentials
                .get("api_key")
                .map(String::as_str)
                .unwrap_or_default();
            if self.embed_credential {
                Ok(format!("{input}; key={secret}"))
            } else {
                Ok(input.to_string())
            }
        }
    }

    // --- Allowlist ----------------------------------------------------------

    #[test]
    fn empty_registry_denies_everything() {
        let registry = ToolRegistry::new();
        assert!(registry.is_empty());
        assert!(!registry.is_allowed("web_search"));
        assert!(registry.spec("web_search").is_none());
    }

    #[tokio::test]
    async fn empty_executor_denies_everything() {
        let executor = ToolExecutor::new();
        assert!(executor.registry().is_empty());
        let ctx = ToolContext::new("task-1", "worker-1");
        let err = executor
            .execute("anything", "in", &ctx)
            .await
            .expect_err("empty allowlist must deny");
        assert_eq!(
            err,
            ToolError::ToolNotAllowed {
                tool: "anything".into()
            }
        );
        assert_eq!(err.code(), "tool_not_allowed");
    }

    #[tokio::test]
    async fn unregistered_tool_denied_with_typed_error() {
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(EchoTool::read_only("web_search")))
            .expect("first registration");
        let ctx = ToolContext::new("task-1", "worker-1");
        let err = executor
            .execute("shell", "rm -rf /", &ctx)
            .await
            .expect_err("not registered");
        assert!(matches!(err, ToolError::ToolNotAllowed { .. }));
        assert_eq!(err.code(), "tool_not_allowed");
        assert!(err.to_string().contains("shell"));
        assert!(err.to_string().contains("allowlist"));
        // The registered tool still executes.
        assert_eq!(
            executor.execute("web_search", "q", &ctx).await.unwrap(),
            "q"
        );
    }

    #[tokio::test]
    async fn registered_tool_executes() {
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(EchoTool::read_only("web_search")))
            .expect("first registration");
        assert!(executor.registry().is_allowed("web_search"));
        let spec = executor.registry().spec("web_search").unwrap();
        assert_eq!(spec.name, "web_search");
        assert_eq!(spec.version, "1.0");
        let ctx = ToolContext::new("task-1", "worker-1");
        assert_eq!(
            executor
                .execute("web_search", "weather", &ctx)
                .await
                .unwrap(),
            "weather"
        );
    }

    #[test]
    fn registry_duplicate_registration_rejected_first_wins() {
        let mut registry = ToolRegistry::new();
        registry
            .register(ToolSpec::new("web_search", "1.0"))
            .expect("first registration");
        let err = registry
            .register(ToolSpec::new("web_search", "2.0"))
            .expect_err("duplicate must be rejected");
        assert_eq!(err.code(), "tool_already_registered");
        // Nothing changed: the first registration survives.
        assert_eq!(registry.spec("web_search").unwrap().version, "1.0");
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn executor_duplicate_registration_rejected_nothing_changes() {
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(EchoTool::read_only("web_search")))
            .expect("first registration");
        let err = executor
            .register_tool(Arc::new(EchoTool {
                spec: ToolSpec::new("web_search", "9.9"),
                embed_credential: false,
            }))
            .expect_err("duplicate must be rejected");
        assert_eq!(err.code(), "tool_already_registered");
        // The first tool still runs with its original spec.
        assert_eq!(
            executor.registry().spec("web_search").unwrap().version,
            "1.0"
        );
        let ctx = ToolContext::new("task-1", "worker-1");
        assert!(executor.execute("web_search", "x", &ctx).await.is_ok());
    }

    // --- Approval gate ------------------------------------------------------

    #[tokio::test]
    async fn side_effect_tool_without_approval_denied() {
        // Side effect WITHOUT the explicit approval flag: the union gate
        // still blocks it (审批前禁止执行有副作用工具).
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(EchoTool {
                spec: ToolSpec {
                    name: "payment_charge".into(),
                    version: "1.0".into(),
                    has_side_effects: true,
                    requires_approval: false,
                },
                embed_credential: false,
            }))
            .expect("first registration");
        let ctx = ToolContext::new("task-1", "worker-1").approved(false);
        let err = executor
            .execute("payment_charge", "100 USD", &ctx)
            .await
            .expect_err("side effects need approval");
        assert!(matches!(err, ToolError::ToolRequiresApproval { .. }));
        assert_eq!(err.code(), "tool_requires_approval");
        assert!(err.to_string().contains("payment_charge"));
        // Guidance names the v1 approval input and the E5-2 forward ref.
        assert!(err.to_string().contains("__approval"));
        assert!(err.to_string().contains("E5-2"));
    }

    #[tokio::test]
    async fn side_effect_tool_with_approval_executes() {
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(EchoTool {
                spec: ToolSpec::new("payment_charge", "1.0").side_effects(),
                embed_credential: false,
            }))
            .expect("first registration");
        let ctx = ToolContext::new("task-1", "worker-1").approved(true);
        assert_eq!(
            executor
                .execute("payment_charge", "100 USD", &ctx)
                .await
                .unwrap(),
            "100 USD"
        );
    }

    #[tokio::test]
    async fn approval_flag_gates_read_only_tool_too() {
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(EchoTool {
                spec: ToolSpec::new("expensive_report", "1.0").requires_approval(),
                embed_credential: false,
            }))
            .expect("first registration");
        let ctx = ToolContext::new("task-1", "worker-1");
        let err = executor
            .execute("expensive_report", "q3", &ctx)
            .await
            .expect_err("approval-requiring tool needs approval");
        assert_eq!(err.code(), "tool_requires_approval");
        // With approval it runs.
        let approved = ToolContext::new("task-1", "worker-1").approved(true);
        assert_eq!(
            executor
                .execute("expensive_report", "q3", &approved)
                .await
                .unwrap(),
            "q3"
        );
    }

    #[tokio::test]
    async fn read_only_tool_without_approval_executes() {
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(EchoTool::read_only("web_search")))
            .expect("first registration");
        let ctx = ToolContext::new("task-1", "worker-1");
        assert_eq!(
            executor.execute("web_search", "q", &ctx).await.unwrap(),
            "q"
        );
    }

    // --- __approval task input ----------------------------------------------

    #[test]
    fn task_input_approval_from_plain_true() {
        let mut vars = HashMap::new();
        vars.insert(APPROVAL_VARIABLE.to_string(), "true".to_string());
        assert!(TaskInput::from_task_variables(&vars).approval_granted());
    }

    #[test]
    fn task_input_approval_absent_or_false() {
        // Absent -> not approved.
        assert!(!TaskInput::from_task_variables(&HashMap::new()).approval_granted());
        // Explicit false -> not approved.
        let mut vars = HashMap::new();
        vars.insert(APPROVAL_VARIABLE.to_string(), "false".to_string());
        assert!(!TaskInput::from_task_variables(&vars).approval_granted());
    }

    #[test]
    fn task_input_approval_non_exact_values_fail_closed() {
        for value in ["TRUE", "True", " true", "true ", "1", "yes"] {
            let mut vars = HashMap::new();
            vars.insert(APPROVAL_VARIABLE.to_string(), value.to_string());
            assert!(
                !TaskInput::from_task_variables(&vars).approval_granted(),
                "'{value}' must not count as approval (exact plain \"true\" only)"
            );
        }
        // An envelope value is not the plain contract form: not approved.
        let mut vars = HashMap::new();
        vars.insert(
            APPROVAL_VARIABLE.to_string(),
            r#"__var:1:{"t":"json","val":true}"#.to_string(),
        );
        assert!(!TaskInput::from_task_variables(&vars).approval_granted());
    }

    // --- Credentials --------------------------------------------------------

    // Unique env var names per test: std::env is process-global and tests
    // run in parallel.
    const ENV_PRESENT: &str = "BPM_TEST_TOOL_CRED_PRESENT_E33A";
    const ENV_MISSING: &str = "BPM_TEST_TOOL_CRED_MISSING_E33A";
    const ENV_LEAK_CANARY: &str = "BPM_TEST_TOOL_CRED_CANARY_E33B";

    #[test]
    fn credential_injection_resolves_from_env() {
        std::env::set_var(ENV_PRESENT, "tok_12345");
        let injector = CredentialInjector::new()
            .source(CredentialSource::env(ENV_PRESENT))
            .source(CredentialSource::env(ENV_PRESENT).inject_as("renamed_key"));
        let credentials = injector.resolve().expect("env var present");
        assert_eq!(
            credentials.get(ENV_PRESENT).map(String::as_str),
            Some("tok_12345")
        );
        assert_eq!(
            credentials.get("renamed_key").map(String::as_str),
            Some("tok_12345")
        );
        assert_eq!(injector.sources().len(), 2);
        std::env::remove_var(ENV_PRESENT);
    }

    #[test]
    fn missing_env_var_error_names_var_never_value() {
        // A DIFFERENT secret IS present; the error must not leak it.
        std::env::set_var(ENV_LEAK_CANARY, "super-secret-canary-value");
        std::env::remove_var(ENV_MISSING);
        let injector = CredentialInjector::new()
            .source(CredentialSource::env(ENV_LEAK_CANARY))
            .source(CredentialSource::env(ENV_MISSING).inject_as("api_key"));
        let err = injector.resolve().expect_err("missing env var");
        assert_eq!(
            err,
            ToolError::CredentialMissing {
                env_var: ENV_MISSING.into()
            }
        );
        assert_eq!(err.code(), "credential_missing");
        assert!(err.to_string().contains(ENV_MISSING));
        // The present credential's VALUE never appears in the error.
        assert!(!err.to_string().contains("super-secret-canary-value"));
        std::env::remove_var(ENV_LEAK_CANARY);
    }

    #[test]
    fn mask_credentials_in_replaces_occurrences() {
        let mut credentials = HashMap::new();
        credentials.insert("api_key".to_string(), "tok_12345".to_string());
        let masked = mask_credentials_in("call tok_12345 then tok_12345 again", &credentials);
        assert_eq!(masked, "call *** then *** again");
        // Unrelated values pass through.
        assert_eq!(
            mask_credentials_in("no secrets here", &credentials),
            "no secrets here"
        );
    }

    #[test]
    fn mask_credentials_in_longest_first_and_skips_empty() {
        let mut credentials = HashMap::new();
        credentials.insert("short".to_string(), "secret".to_string());
        credentials.insert("long".to_string(), "secret123".to_string());
        credentials.insert("empty".to_string(), String::new());
        let masked = mask_credentials_in("a secret123 b secret c", &credentials);
        // Longest first: "secret123" must not degrade into "***123".
        assert_eq!(masked, "a *** b *** c");
    }

    #[test]
    fn redact_string_masks_literal_substrings() {
        let masked = redact_string(
            "bearer abc.def.ghi and abc.def.ghi twice",
            &["abc.def.ghi".to_string()],
        );
        assert_eq!(masked, "bearer *** and *** twice");
        // Empty patterns are skipped (a bare "" would match everywhere).
        assert_eq!(redact_string("value", &["".to_string()]), "value");
    }

    // --- Output limits ------------------------------------------------------

    #[test]
    fn output_at_limit_is_verbatim() {
        let limits = OutputLimits { max_bytes: 10 };
        let at_limit = "x".repeat(10);
        assert_eq!(apply_output_limit("t", &at_limit, &limits), at_limit);
        assert_eq!(apply_output_limit("t", "", &limits), "");
    }

    #[test]
    fn output_over_limit_becomes_exact_core_marker() {
        // Known vector: SHA-256("hello") starts 2cf24dba5fb0 — core's own
        // contract test uses the same digest.
        let limits = OutputLimits { max_bytes: 4 };
        assert_eq!(apply_output_limit("t", "hello", &limits), HELLO_MARKER);
        assert_eq!(HELLO_MARKER, "__truncated:5:2cf24dba5fb0:");
    }

    #[test]
    fn output_limit_marker_parity_with_core_redaction() {
        // PARITY: for the same cap, apply_output_limit must return exactly
        // what core's redact_history_variable_value returns — it delegates
        // to that function, and this test pins the delegation.
        let values = [
            "",
            "hello",
            &"x".repeat(64),
            &"y".repeat(65),
            &"z".repeat(1024),
            &"w".repeat(1025),
            r#"__var:2:{"t":"json","val":1}"#,
            r#"__var:1:{"t":"json","val":7}"#,
            "__encrypted:QUFB:QkJC",
        ];
        let caps = [0usize, 4, 10, 64, 1024, DEFAULT_TOOL_OUTPUT_MAX_BYTES];
        for cap in caps {
            let limits = OutputLimits { max_bytes: cap };
            let policy = HistoryRedactionPolicy {
                max_variable_bytes: cap,
            };
            for value in values {
                assert_eq!(
                    apply_output_limit("t", value, &limits),
                    redact_history_variable_value(value, &policy),
                    "parity broken for cap={cap} value_len={}",
                    value.len()
                );
            }
        }
    }

    #[test]
    fn default_output_limit_is_64_kib_matching_engine_cap() {
        assert_eq!(OutputLimits::default().max_bytes, 65_536);
        assert_eq!(
            OutputLimits::default().max_bytes,
            DEFAULT_TOOL_OUTPUT_MAX_BYTES
        );
    }

    // --- Redaction ----------------------------------------------------------

    #[test]
    fn redact_variables_exact_name_match() {
        let mut map = HashMap::new();
        map.insert("api_key".to_string(), "tok_12345".to_string());
        map.insert("verdict".to_string(), "approved".to_string());
        let policy = RedactionPolicy::default().redact_variable_names(["api_key"]);
        let redacted = redact_variables(&map, &policy);
        assert_eq!(redacted.get("api_key").map(String::as_str), Some("***"));
        assert_eq!(
            redacted.get("verdict").map(String::as_str),
            Some("approved")
        );
    }

    #[test]
    fn redact_variables_prefix_wildcard() {
        let mut map = HashMap::new();
        map.insert("token_a".to_string(), "1".to_string());
        map.insert("token_b".to_string(), "2".to_string());
        map.insert("token".to_string(), "3".to_string()); // No trailing underscore.
        map.insert("my_token".to_string(), "4".to_string()); // Prefix must be at the start.
        let policy = RedactionPolicy::default().redact_variable_names(["token_*"]);
        let redacted = redact_variables(&map, &policy);
        assert_eq!(redacted.get("token_a").map(String::as_str), Some("***"));
        assert_eq!(redacted.get("token_b").map(String::as_str), Some("***"));
        assert_eq!(redacted.get("token").map(String::as_str), Some("3"));
        assert_eq!(redacted.get("my_token").map(String::as_str), Some("4"));
    }

    #[test]
    fn redact_variables_bare_star_redacts_everything() {
        let mut map = HashMap::new();
        map.insert("anything".to_string(), "v".to_string());
        let policy = RedactionPolicy::default().redact_variable_names(["*"]);
        let redacted = redact_variables(&map, &policy);
        assert_eq!(redacted.get("anything").map(String::as_str), Some("***"));
    }

    #[test]
    fn redact_variables_custom_mask() {
        let mut map = HashMap::new();
        map.insert("secret".to_string(), "v".to_string());
        let policy = RedactionPolicy {
            redact_variable_names: vec!["secret".to_string()],
            mask: "[redacted]".to_string(),
        };
        let redacted = redact_variables(&map, &policy);
        assert_eq!(
            redacted.get("secret").map(String::as_str),
            Some("[redacted]")
        );
    }

    #[test]
    fn redact_variables_matches_are_case_sensitive() {
        let mut map = HashMap::new();
        map.insert("API_KEY".to_string(), "tok".to_string());
        let policy = RedactionPolicy::default().redact_variable_names(["api_key"]);
        let redacted = redact_variables(&map, &policy);
        assert_eq!(redacted.get("API_KEY").map(String::as_str), Some("tok"));
    }

    // --- TaskOutput integration ---------------------------------------------

    #[tokio::test]
    async fn add_tool_output_applies_redaction_then_limit() {
        let mut credentials = HashMap::new();
        credentials.insert("api_key".to_string(), "tok_12345".to_string());
        let ctx = ToolContext::new("task-1", "worker-1")
            .credentials(credentials)
            .output_limits(OutputLimits { max_bytes: 20 })
            .redaction(RedactionPolicy::default().redact_variable_names(["token_*"]));

        // Credential inside the value + over the cap: masked first, then the
        // marker digests the MASKED value (never a secret-bearing string).
        let value = format!("tok_12345 and {}", "x".repeat(30));
        let variables = TaskOutput::new()
            .add_tool_output("summary", &value, &ctx)
            .add_tool_output("token_result", "small but sensitive", &ctx)
            .build()
            .expect("no envelope");
        let limited = variables.get("summary").unwrap();
        assert!(limited.starts_with("__truncated:"), "got {limited:?}");
        assert!(!limited.contains("tok_12345"));
        // The digest in the marker is of the masked value: verify by
        // applying the same pipeline manually.
        let masked = "*** and ".to_string() + &"x".repeat(30);
        let expected = apply_output_limit("summary", &masked, &ctx.output_limits);
        assert_eq!(*limited, expected);
        // Sensitive name: whole value masked regardless of content.
        assert_eq!(
            variables.get("token_result").map(String::as_str),
            Some("***")
        );
    }

    #[tokio::test]
    async fn add_tool_output_under_limit_passes_verbatim() {
        let ctx =
            ToolContext::new("task-1", "worker-1").output_limits(OutputLimits { max_bytes: 100 });
        let variables = TaskOutput::new()
            .add_tool_output("verdict", "approved", &ctx)
            .build()
            .expect("no envelope");
        assert_eq!(
            variables.get("verdict").map(String::as_str),
            Some("approved")
        );
    }

    // --- Error mapping ------------------------------------------------------

    #[test]
    fn tool_error_maps_to_task_result_fail_with_code() {
        for err in [
            ToolError::ToolNotAllowed {
                tool: "shell".into(),
            },
            ToolError::ToolRequiresApproval {
                tool: "payment_charge".into(),
            },
            ToolError::CredentialMissing {
                env_var: "MISSING_VAR".into(),
            },
            ToolError::ToolExecutionFailed {
                tool: "web_search".into(),
                reason: "boom".into(),
            },
        ] {
            let code = err.code();
            let message = err.to_string();
            let result: TaskResult = err.into();
            match result {
                TaskResult::Fail { error, retry_after } => {
                    assert!(error.starts_with(&format!("{code}: ")), "got {error:?}");
                    assert!(error.contains(&message), "got {error:?}");
                    assert!(retry_after.is_none());
                }
                TaskResult::Complete { .. } => panic!("tool errors must map to Fail"),
            }
        }
    }

    #[test]
    fn tool_spec_projects_to_envelope_tool_ref() {
        let spec = ToolSpec::new("web_search", "2.1").side_effects();
        let tool_ref = ToolRef::from(&spec);
        assert_eq!(tool_ref.name, "web_search");
        assert_eq!(tool_ref.version, Some("2.1".to_string()));
        // Empty version maps to None (stays valid for the envelope).
        let no_version = ToolRef::from(&ToolSpec::new("x", ""));
        assert_eq!(no_version.version, None);
    }

    // --- End-to-end-ish handler composition ---------------------------------

    /// A realistic TaskHandler: input approval, env credentials, allowlisted
    /// side-effect tool, limited + redacted output, tool recorded in the AI
    /// task envelope.
    struct ChargeHandler {
        executor: ToolExecutor,
        injector: CredentialInjector,
    }

    struct ChargeTool;

    #[async_trait]
    impl Tool for ChargeTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::new("payment_charge", "1.4").side_effects()
        }
        async fn run(&self, input: &str, ctx: &ToolContext) -> Result<String, ToolError> {
            // Idempotency key from the persistent ids (at-least-once).
            let _idempotency_key = format!("{}:{}", ctx.worker_id, ctx.task_id);
            let key = ctx
                .credentials
                .get("payment_api_key")
                .cloned()
                .unwrap_or_default();
            // The raw output embeds the credential: the boundary must strip it.
            Ok(format!("charged {input} with {key} ")).map(|out| out + &"detail ".repeat(20))
        }
    }

    #[async_trait]
    impl TaskHandler for ChargeHandler {
        fn task_type(&self) -> &str {
            "payment_charge"
        }

        async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
            let input = TaskInput::from_task_variables(&task.variables);
            let credentials = match self.injector.resolve() {
                Ok(c) => c,
                Err(e) => return e.into(),
            };
            let tool_ctx = ToolContext::new(&task.task_id, ctx.worker_id())
                .approved(input.approval_granted())
                .credentials(credentials)
                .output_limits(OutputLimits { max_bytes: 60 });
            let amount = input.plain("amount").unwrap_or("0");
            let raw = match self
                .executor
                .execute("payment_charge", amount, &tool_ctx)
                .await
            {
                Ok(out) => out,
                Err(e) => return e.into(), // <- tool errors -> TaskResult::Fail
            };
            let output = TaskOutput::new().add_tool_output("charge_result", &raw, &tool_ctx);
            let mut envelope = crate::ai_task::new_envelope(&task.task_id, 0);
            envelope.tools = self
                .executor
                .registry()
                .spec("payment_charge")
                .map(|spec| vec![ToolRef::from(spec)])
                .unwrap_or_default();
            match output.envelope(envelope).build() {
                Ok(variables) => TaskResult::Complete { variables },
                Err(e) => TaskResult::Fail {
                    error: format!("{}: {}", e.code(), e),
                    retry_after: None,
                },
            }
        }
    }

    fn charge_task(approved: bool) -> ExternalTask {
        let mut variables = HashMap::new();
        variables.insert("amount".to_string(), "100 USD".to_string());
        variables.insert(
            APPROVAL_VARIABLE.to_string(),
            if approved { "true" } else { "false" }.to_string(),
        );
        ExternalTask {
            task_id: "task-charge-1".to_string(),
            task_type: "payment_charge".to_string(),
            variables,
            lock_expire_at: None,
            retries: 0,
            #[cfg(debug_assertions)]
            token_id: None,
            #[cfg(debug_assertions)]
            process_instance_id: None,
        }
    }

    /// Each handler test uses its OWN env var name: `std::env` is
    /// process-global and tests run in parallel, so shared names would race
    /// (and a lock held across the handler's awaits is not an option).
    fn charge_handler(env_var: &str) -> ChargeHandler {
        let mut executor = ToolExecutor::new();
        executor
            .register_tool(Arc::new(ChargeTool))
            .expect("first registration");
        let injector = CredentialInjector::new()
            .source(CredentialSource::env(env_var).inject_as("payment_api_key"));
        ChargeHandler { executor, injector }
    }

    const HANDLER_ENV_OK: &str = "BPM_TEST_TOOL_CRED_E33_OK";
    const HANDLER_ENV_NO_APPROVAL: &str = "BPM_TEST_TOOL_CRED_E33_NO_APPROVAL";
    const HANDLER_ENV_MISSING: &str = "BPM_TEST_TOOL_CRED_E33_MISSING";

    #[tokio::test]
    async fn handler_completes_with_limited_and_redacted_output() {
        std::env::set_var(HANDLER_ENV_OK, "tok_secret_charge");
        let handler = charge_handler(HANDLER_ENV_OK);
        // TaskContext needs a client but never contacts it in this test.
        let ctx = TaskContext::new(
            "worker-1".to_string(),
            "task-charge-1".to_string(),
            EngineClient::new("http://127.0.0.1:1"),
        );
        let result = handler.handle(charge_task(true), ctx).await;
        let TaskResult::Complete { variables } = result else {
            panic!("approved charge must complete, got {result:?}");
        };
        let charge_result = variables.get("charge_result").expect("output present");
        // Over the 60-byte cap -> the core truncation marker.
        assert!(
            charge_result.starts_with("__truncated:"),
            "got {charge_result:?}"
        );
        // The credential never survives into the submitted variables.
        assert!(!charge_result.contains("tok_secret_charge"));
        // The envelope rides along and records the executed tool.
        let envelope = crate::ai_task::TaskInput::from_task_variables(&variables)
            .envelope()
            .expect("envelope present")
            .expect("valid envelope");
        assert_eq!(envelope.tools.len(), 1);
        assert_eq!(envelope.tools[0].name, "payment_charge");
        assert_eq!(envelope.tools[0].version, Some("1.4".to_string()));
        std::env::remove_var(HANDLER_ENV_OK);
    }

    #[tokio::test]
    async fn handler_fails_side_effect_tool_without_approval() {
        // The credential must be PRESENT here so the asserted failure is the
        // approval gate, not a missing credential.
        std::env::set_var(HANDLER_ENV_NO_APPROVAL, "tok_secret_charge");
        let handler = charge_handler(HANDLER_ENV_NO_APPROVAL);
        let ctx = TaskContext::new(
            "worker-1".to_string(),
            "task-charge-1".to_string(),
            EngineClient::new("http://127.0.0.1:1"),
        );
        let result = handler.handle(charge_task(false), ctx).await;
        match result {
            TaskResult::Fail { error, .. } => {
                assert!(
                    error.starts_with("tool_requires_approval: "),
                    "got {error:?}"
                );
                assert!(error.contains("payment_charge"));
            }
            TaskResult::Complete { .. } => panic!("unapproved side-effect tool must fail"),
        }
    }

    #[tokio::test]
    async fn handler_fails_when_credential_env_var_missing() {
        std::env::remove_var(HANDLER_ENV_MISSING);
        let handler = charge_handler(HANDLER_ENV_MISSING);
        let ctx = TaskContext::new(
            "worker-1".to_string(),
            "task-charge-1".to_string(),
            EngineClient::new("http://127.0.0.1:1"),
        );
        let result = handler.handle(charge_task(true), ctx).await;
        match result {
            TaskResult::Fail { error, .. } => {
                assert!(error.starts_with("credential_missing: "), "got {error:?}");
                assert!(error.contains(HANDLER_ENV_MISSING));
            }
            TaskResult::Complete { .. } => panic!("missing credential must fail"),
        }
    }
}
