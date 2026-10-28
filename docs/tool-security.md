---
layout: default
---

# Tool Security — Worker-Side Boundary Contract (E3-3)

> This is the **contract reference for AI workers** that invoke tools (HTTP
> calls, code, file access) while handling external tasks. The
> implementation is `bpm_engine_worker_sdk::tools`. The SDK provides the
> **security boundary**, not the tools: workers implement
> `Tool` themselves and get allowlisting, the approval gate, credential
> injection, output limits and redaction enforced around their code.
>
> Why the worker side? The engine never dereferences artifact URIs and never
> runs tools — every side effect of an AI-driven external task happens inside
> a worker process (`docs/recovery.md` §8). A boundary anywhere else would
> not be where the risk is.

Related contracts: `docs/ai-task-envelope.md` (E3-1, typed execution
metadata, including the `tools` a task used), `docs/variables-and-artifacts.md`
(E2-3, the variable value format and size caps this module aligns with).

---

## 1. The four enforced properties

| # | Property (issue #17) | Enforcement | On violation |
|---|----------------------|-------------|--------------|
| 1 | 工具 allowlist | `ToolExecutor`: only registered tools execute; an empty executor denies everything | `tool_not_allowed` |
| 2 | 审批前禁止副作用工具 | approval gate before dispatch | `tool_requires_approval` |
| 3 | 凭证注入 | `CredentialInjector` reads the worker process environment only | `credential_missing` |
| 4 | 输出大小限制 / 敏感信息脱敏 | `apply_output_limit` + `redact_variables` / `mask_credentials_in`, applied by `TaskOutput::add_tool_output` | (not errors: outputs are transformed) |

Tool failures themselves surface as `tool_execution_failed`.

## 2. Allowlist — deny by default

```rust
use bpm_engine_worker_sdk::tools::{Tool, ToolContext, ToolError, ToolExecutor, ToolSpec};

let mut executor = ToolExecutor::new();          // empty: every tool denied
executor.register_tool(Arc::new(MySearchTool))   // spec + implementation together
    .expect("first registration");

executor.registry().is_allowed("web_search");    // the enforced allowlist
executor.registry().names();                     // e.g. tools to tell a model about
```

- **`ToolExecutor::new()` denies every tool.** Only
  `register_tool(Arc<dyn Tool>)` puts a tool behind the boundary, and it
  registers the implementation **and** its `ToolSpec` in one step — the
  enforced allowlist (`registry()`) can never diverge from the runnable set.
  There is deliberately no "adopt a spec list without an implementation"
  path, because a declared-but-unimplemented tool would make `is_allowed`
  lie about what can actually run.
- **`ToolRegistry` is also a standalone type** (`register(spec)` /
  `is_allowed(name)` / `spec(name)`) for cheap pre-checks and for building
  the tool list a worker advertises to a model. It is an observation aid;
  enforcement always goes through the executor.
- **Duplicate registration is rejected** with `tool_already_registered` and
  changes nothing (first registration wins — no silent replacement, no
  panic); **empty or whitespace-only names are rejected** with
  `invalid_tool_name`. These are registration-time codes, not
  execution-boundary codes.
- `ToolSpec` carries `name`, `version`, `has_side_effects`,
  `requires_approval`. `From<&ToolSpec> for ToolRef` projects it into the
  E3-1 envelope's `tools` so a worker can record what ran
  (`docs/ai-task-envelope.md` §2).

## 3. Approval gate — 审批前禁止副作用工具

`ToolExecutor::execute(name, input, ctx)` checks, in order:

1. **Allowlist**: unregistered name → `tool_not_allowed`.
2. **Approval gate**: if `(requires_approval || has_side_effects)` and
   `!ctx.approved` → `tool_requires_approval`.
3. Dispatch to the implementation.

The gate fires when the tool is declared side-effectful **or**
approval-requiring. `requires_approval` alone is for gating side-effect-free
but sensitive/expensive reads; `has_side_effects` **always** implies the gate
— the SDK refuses to run a side-effectful tool before approval even if the
spec left `requires_approval` false (fail-closed; `ToolSpec::side_effects()`
sets both flags). The error message names the tool and points at the
approval input:

> tool 'payment_charge' must not execute before approval: it has side
> effects and this task is not approved. Grant approval via the task input
> variable `__approval` = "true" — for approval-required task types
> (E5-2) the engine sets it from the persisted approval decision; for
> other types it still arrives through task variables

### 3.1 The `__approval` input variable (E5-2: engine-written truth)

| | |
|---|---|
| key | `__approval` (`bpm_engine_worker_sdk::tools::APPROVAL_VARIABLE`) |
| form | plain legacy string, exactly `"true"` or `"false"` |
| absent | not approved |
| read | `TaskInput::approval_granted()` — fail-closed: `"TRUE"`, `" true"`, `"1"`, `__var:…` envelope forms → **not** approved |
| who sets it (approval-required task types, E5-2) | **the engine**: an operator approves the persisted `ApprovalRecord` (`POST /api/v1/approvals/:id/decide`) and the engine creates the fetchable task with `__approval=true` injected — task-creation-after-approval means workers never see unapproved work at all |
| who sets it (other task types) | a human approver, through task variables (the pre-E5-2 convention — unchanged) |

Workers derive the gate input from the task, never from their own opinion:

```rust
let input = TaskInput::from_task_variables(&task.variables);
let tool_ctx = ToolContext::new(&task.task_id, ctx.worker_id())
    .approved(input.approval_granted());   // <- the only sanctioned source
```

The pre-E5-2 forgeability caveat is **resolved for approval-required
types**: the engine overwrites any client-supplied `__approval` with its
own truth when it creates the gated task (engine truth wins, ADR-008 §3),
and until approval the task does not exist. Residual boundary (honest):
task types NOT in `engine.approval_required_task_types` still rely on the
worker-side convention — a task-variable writer can still set
`__approval` for them. Move a type into the config set to put it behind
the engine gate. Full semantics: `docs/approval-semantics.md`.

The SDK enforces the DECLARED flags only — it cannot observe a tool's real
effects. A worker author who mislabels a side-effectful tool as
`has_side_effects = false, requires_approval = false` bypasses the gate;
correct `ToolSpec` declaration is the worker author's responsibility (the
same trust boundary as the `Tool` implementation itself).

## 4. Credentials — process environment only

```rust
let injector = CredentialInjector::new()
    .source(CredentialSource::env("PAYMENT_API_KEY").inject_as("payment_api_key"));
let credentials = injector.resolve()?;   // HashMap for ToolContext
```

- **Sources are env vars, nothing else** (v1 has the single `Env` variant).
  Task variables are explicitly NOT a credential source: they are persisted
  (instance state, task rows, event history — `docs/variables-and-artifacts.md`
  §6), redelivered after lease expiry, and delivered over the wire, so a
  secret in them leaks into storage, history and the DLQ. The worker
  environment stays inside the worker process.
- A missing (or non-UTF-8) env var → `credential_missing`, naming the env
  var — never a value. Credential **values are never logged** by this
  module. Resolve at worker startup where possible so a deployment error
  fails the boot, not every task.
- Tool implementations read `ctx.credentials` (injected via
  `ToolContext::credentials`). Any occurrence of a credential value in an
  output or log must be masked first:
  `mask_credentials_in(value, &ctx.credentials)` → `"***"`.
  `TaskOutput::add_tool_output` applies this automatically.

## 5. Output limits — marker parity with the engine

`OutputLimits { max_bytes }` defaults to **64 KiB** (65_536), matching the
engine's `variable_max_bytes` cap enforced at the REST write boundary (4xx
`variable_too_large`, `docs/variables-and-artifacts.md` §4). A value above
the server's cap would be rejected after submission, losing the whole
completion — so the worker truncates **before** submitting. Align
`max_bytes` with the server's configured cap when it differs.

`apply_output_limit(name, value, limits)`:

- at or below the cap → verbatim;
- over the cap → `__truncated:<orig_len>:<sha256-first-12-hex>:` — **the
  exact E2-3 history marker**, because the function delegates to core's
  `redact_history_variable_value` (`bpm_engine_core::variable_contract`) —
  same code path, parity by construction, pinned by a test that asserts
  identical output for a battery of inputs and caps, plus the known
  SHA-256("hello") vector (`__truncated:5:2cf24dba5fb0:`).
- a value that happens to be an unknown envelope version (`__var:2:…`) maps
  to `__opaque:` exactly like the core contract (never interpret).

`TaskOutput::add_tool_output(name, value, ctx)` applies the full pipeline —
**redaction before the limit** (see §6 for why the ordering matters):

1. name-based redaction (a sensitive variable name masks the value),
2. credential substring masking,
3. the output limit (whose digest is then computed over the redacted value).

## 6. Sensitive redaction — name-based and substring

Two complementary, regex-free mechanisms:

| Function | Masks | By |
|----------|-------|-----|
| `redact_variables(map, policy)` | whole VALUES | variable NAME matching `RedactionPolicy.redact_variable_names` |
| `redact_string(value, patterns)` / `mask_credentials_in(value, credentials)` | literal SUBSTRINGS inside a value | the pattern/credential values themselves |

- Name patterns: exact (`"api_key"`) or trailing-wildcard prefix
  (`"token_*"` matches `token_a`, not `token` or `my_token`; a bare `"*"`
  redacts everything). Matching is case-sensitive; a `*` anywhere but the
  end is literal. Custom mask via `RedactionPolicy.mask` (default `"***"`).
- Substring masking replaces every occurrence with `"***"`, longest pattern
  first (overlapping secrets cannot survive as partial remnants) and skips
  empty patterns/values.
- **Ordering rule**: redact, THEN limit. The `__truncated:` marker embeds a
  digest of the value it replaces — computing it over a secret-bearing
  string would publish an offline-verifiable fingerprint of that secret.
  `add_tool_output` guarantees the ordering; keep it if you build the
  pipeline manually.
- **No regex is a deliberate tradeoff**: literal/Exact and prefix matching
  is predictable, dependency-free and auditable in a security boundary; the
  pattern vocabulary is limited (no character classes, no alternation). If a
  future need justifies regex, it must arrive with a ReDoS analysis.

## 7. Error codes (stable, worker → engine)

`ToolError::code()` — snake_case, mirroring the E2-3/E3-1 conventions:

| Code | When | Message carries |
|------|------|-----------------|
| `tool_not_allowed` | execute on an unregistered tool (deny by default) | the tool name |
| `tool_requires_approval` | side-effect/approval-requiring tool without task approval | the tool name, the `__approval` input, the E5-2 forward reference |
| `credential_missing` | env var absent (or non-UTF-8) at resolve time | the env var NAME, never a value |
| `tool_execution_failed` | a registered, approved tool failed | the tool name and the implementation's reason |
| `tool_already_registered` | duplicate registration (boot-time, rejected, nothing changed) | the tool name |
| `invalid_tool_name` | registration with an empty or whitespace-only name (boot-time, rejected) | — |

`From<ToolError> for TaskResult` maps any of these to
`TaskResult::Fail { error: "<code>: <message>", retry_after: None }` — the
code leads the message (the same shape as REST error bodies) so it survives
into the engine's `error_message`, the `ExternalTaskFailed` history payload
and the DLQ (E3-1 §8). `retry_after` is `None` (engine default schedule):
`tool_not_allowed` / `tool_requires_approval` / `credential_missing` are
permanent until the task inputs or the worker environment change — acting
on that distinction is E3-2's retry-category scope.

## 8. At-least-once means idempotent tools

External task side effects execute **at-least-once** (lease expiry →
redelivery, worker crash → redelivery; `docs/recovery.md` §8). A tool with
`has_side_effects` therefore must be **idempotent**: derive idempotency keys
from the persistent ids in `ToolContext` (`task_id`, `worker_id`) or the
E3-1 envelope's `execution_id` / `idempotency_key`. The SDK enforces the
boundary; idempotency remains the tool author's responsibility, as with any
`TaskHandler`.

## 9. Reference worker composition

```rust
#[async_trait]
impl TaskHandler for ChargeHandler {
    fn task_type(&self) -> &str { "payment_charge" }

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
        let input = TaskInput::from_task_variables(&task.variables);
        let tool_ctx = ToolContext::new(&task.task_id, ctx.worker_id())
            .approved(input.approval_granted())      // from __approval
            .credentials(match self.injector.resolve() { Ok(c) => c, Err(e) => return e.into() });
        // Boundary: allowlist + approval gate enforced here.
        let raw = match self.executor.execute("payment_charge", input.plain("amount").unwrap_or("0"), &tool_ctx).await {
            Ok(out) => out,
            Err(e) => return e.into(),               // "<code>: <message>" Fail
        };
        let output = TaskOutput::new()
            .add_tool_output("charge_result", &raw, &tool_ctx); // redact + limit
        let mut envelope = new_envelope(&task.task_id, 0);
        envelope.tools = vec![ToolRef::from(self.executor.registry().spec("payment_charge").unwrap())];
        match output.envelope(envelope).build() {
            Ok(variables) => TaskResult::Complete { variables },
            Err(e) => TaskResult::Fail { error: format!("{}: {}", e.code(), e), retry_after: None },
        }
    }
}
```

## 10. Out of scope (v1)

- **No tool implementations ship in the SDK** — the boundary only.
- No regex redaction (§6), no non-env credential sources (vault/secret
  files), no per-tool output-limit overrides (the `ToolContext` limits apply
  to every tool output of the task).
- The engine-side approval flow (E5-2) ships at the ENGINE layer —
  persisted approval records, task-creation-after-approval, and the
  engine-written `__approval` for gated task types (see §3.1 and
  `docs/approval-semantics.md`); the SDK keeps reading the task variable,
  unchanged.
- Retry behavior is unchanged by tool error codes (E3-2).
