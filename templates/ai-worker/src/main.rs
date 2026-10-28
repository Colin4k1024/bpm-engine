//! Copyable external-task worker template for bpm-engine (E4-4, issue #22).
//!
//! What this template wires together — everything an AI-driven worker
//! needs, and nothing else:
//!
//! - the **Rust Worker SDK** poll loop (`Worker::builder()`,
//!   `EngineClient`, `TaskHandler`) — no BPM knowledge in worker code;
//! - the **worker-side tool security boundary** (E3-3,
//!   `docs/tool-security.md`): a side-effectful tool behind the
//!   `ToolExecutor` allowlist (deny by default) and the approval gate
//!   (fail-closed on the `__approval` task input), with credentials read
//!   from the worker's environment ONLY;
//! - the **AI task envelope** (E3-1, `docs/ai-task-envelope.md`): the
//!   completed task carries `__task_envelope` recording the executed tool,
//!   built with `TaskOutput` so the output passes the redaction /
//!   credential-masking / size-limit pipeline before it is persisted.
//!
//! The handler serves task type `ai-act` — the side-effect step of
//! `templates/flows/ai-approval-flow.json`. Swap names, tools and logic
//! for your own; see `README.md` in this directory for the customization
//! walkthrough.
//!
//! Run against a local engine (see docs/quickstart.md):
//!
//! ```text
//! BPM_BASE_URL=http://127.0.0.1:3000 cargo run
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bpm_engine_worker_sdk::ai_task::{new_envelope, TaskInput, TaskOutput, ToolRef};
use bpm_engine_worker_sdk::tools::{
    CredentialInjector, CredentialSource, Tool, ToolContext, ToolError, ToolExecutor, ToolSpec,
};
use bpm_engine_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};

/// The task type this worker fetches. It MUST match the `task_type` of a
/// `serviceTask` node in a deployed flow (e.g. the `act` node of
/// `templates/flows/ai-approval-flow.json`).
const TASK_TYPE: &str = "ai-act";

/// The tool's allowlist key. Only registered tools execute; everything
/// else is denied with `tool_not_allowed`.
const TOOL_NAME: &str = "ledger_post";
const TOOL_VERSION: &str = "1.0";

/// Environment variable the credential injector reads. Credentials come
/// from the worker process environment only — NEVER from task variables
/// (task variables are attacker-controllable input).
const CREDENTIAL_ENV_VAR: &str = "BPM_LEDGER_API_KEY";

/// A side-effectful tool: "post an entry to an external ledger".
///
/// Replace `run` with your real side effect (HTTP call, file write, ...).
/// Handlers run **at-least-once** — make the tool idempotent (keyed by
/// business id or task id), or bring your own dedup/compensation.
struct LedgerPostTool;

#[async_trait]
impl Tool for LedgerPostTool {
    fn spec(&self) -> ToolSpec {
        // side_effects() implies the approval gate: the executor refuses
        // to run this tool before the task is approved (fail-closed).
        ToolSpec::new(TOOL_NAME, TOOL_VERSION).side_effects()
    }

    async fn run(&self, input: &str, ctx: &ToolContext) -> Result<String, ToolError> {
        // The injected credential is available to the implementation.
        // Real tool: an idempotent HTTP POST to the ledger service.
        let key = ctx
            .credentials
            .get("ledger_api_key")
            .cloned()
            .unwrap_or_default();
        Ok(format!("ledger: posted '{input}' with key={key}"))
    }
}

/// The task handler: resolves credentials, runs the tool through the
/// E3-3 boundary, and completes with the E3-1 envelope riding along.
struct ActHandler {
    executor: ToolExecutor,
    injector: CredentialInjector,
}

#[async_trait]
impl TaskHandler for ActHandler {
    fn task_type(&self) -> &str {
        TASK_TYPE
    }

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
        let input = TaskInput::from_task_variables(&task.variables);

        // 1. Credentials: worker environment only. A missing env var is a
        //    configuration error the retry loop cannot fix — the SDK's
        //    `From<ToolError>` mapping fails it PERMANENTLY (straight to
        //    Failed + dead letter, no retries).
        let credentials = match self.injector.resolve() {
            Ok(credentials) => credentials,
            Err(e) => return e.into(),
        };

        // 2. The security boundary: allowlist + approval gate + dispatch.
        //    The approval comes from the task input (`__approval`), never
        //    from the worker's own opinion.
        let tool_ctx = ToolContext::new(&task.task_id, ctx.worker_id())
            .approved(input.approval_granted())
            .credentials(credentials);
        let memo = input.plain("memo").unwrap_or("template-entry");
        let raw = match self.executor.execute(TOOL_NAME, memo, &tool_ctx).await {
            Ok(raw) => raw,
            Err(e) => return e.into(),
        };

        // 3. Output through the security pipeline (redaction, credential
        //    masking, size limit) + the envelope recording the tool.
        let tool_ref = ToolRef::from(
            self.executor
                .registry()
                .spec(TOOL_NAME)
                .expect("the template tool is registered"),
        );
        let mut envelope = new_envelope(&task.task_id, 0);
        envelope.tools = vec![tool_ref];
        let output = TaskOutput::new()
            .add_tool_output("ledger_result", &raw, &tool_ctx)
            .envelope(envelope);

        // `build` validates the envelope client-side, before anything
        // reaches the wire (the server would 4xx with
        // `task_envelope_invalid` otherwise).
        match output.build() {
            Ok(variables) => TaskResult::Complete { variables },
            Err(e) => TaskResult::Fail {
                error: format!("{}: {e}", e.code()),
                retry_after: None,
                category: None,
            },
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let base_url =
        std::env::var("BPM_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".to_string());
    let worker_id = std::env::var("BPM_WORKER_ID").unwrap_or_else(|_| "ai-worker-1".to_string());

    // Deny by default: an empty executor refuses every tool. Registration
    // is the only way in, and it registers implementation + spec together.
    let mut executor = ToolExecutor::new();
    executor
        .register_tool(Arc::new(LedgerPostTool))
        .expect("first registration of the template tool");

    let injector = CredentialInjector::new()
        .source(CredentialSource::env(CREDENTIAL_ENV_VAR).inject_as("ledger_api_key"));

    let worker = Worker::builder()
        .client(EngineClient::new(&base_url))
        .handler(ActHandler { executor, injector })
        .config(
            WorkerConfig::new(worker_id)
                .max_tasks(5)
                .lock_duration(Duration::from_secs(30))
                .poll_interval(Duration::from_secs(1)),
        )
        .build();

    tracing::info!(
        base_url = %base_url,
        task_type = TASK_TYPE,
        tool = TOOL_NAME,
        "ai-worker template starting (Ctrl-C to stop)"
    );
    // Runs until SIGINT/SIGTERM, then shuts down gracefully.
    worker.start().await;
    Ok(())
}
