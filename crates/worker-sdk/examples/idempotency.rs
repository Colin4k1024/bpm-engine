//! Idempotency example: use task_id (or a business key from variables) to avoid double-processing.
//!
//! If the worker crashes after doing work but before calling complete(), the engine will
//! eventually reclaim the lock and another worker may receive the same task. Use an idempotency
//! key (e.g. task_id or instance_id + node_id) to check "already processed?" before doing work.
//!
//! E3-2: the SDK ships `IdempotencyGuard` + `derive_key` for exactly this pattern. The guard is
//! per-process only (bounded in-memory map): it survives redelivery within THIS process's
//! lifetime. Cross-restart / multi-process dedup needs a durable store — see docs/worker-context.md §5
//! and the E3-1 envelope's idempotency_key for the engine-side hook.
//!
//! Run the Engine first: `cargo run -p bpm-engine-server-rest`
//! Then: `cargo run -p bpm-engine-worker-sdk --example idempotency`

use async_trait::async_trait;
use bpm_engine_worker_sdk::{
    derive_key, EngineClient, ExternalTask, IdempotencyGuard, TaskContext, TaskHandler, TaskResult,
    Worker, WorkerConfig,
};
use std::collections::HashMap;
use std::time::Duration;

struct IdempotentPaymentHandler {
    /// Per-process dedup guard (in production, add a durable store).
    guard: tokio::sync::Mutex<IdempotencyGuard>,
}

#[async_trait]
impl TaskHandler for IdempotentPaymentHandler {
    fn task_type(&self) -> &str {
        "payment"
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let amount = task
            .variables
            .get("amount")
            .cloned()
            .unwrap_or_else(|| "0".into());

        // Stable key: same task + same inputs => same key, so a redelivered
        // attempt is recognized (per process — see the module docs).
        let key = derive_key(&task.task_id, &amount);

        let mut guard = self.guard.lock().await;
        if !guard.check_and_mark(&key) {
            tracing::info!(task_id = %task.task_id, "already processed (idempotent), completing");
            let mut variables = HashMap::new();
            variables.insert("status".to_string(), "PAID".to_string());
            return TaskResult::Complete { variables };
        }
        drop(guard);

        tracing::info!(task_id = %task.task_id, amount = %amount, "processing payment");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut variables = HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        variables.insert("amount".to_string(), amount);
        // Side effect done: the key stays marked. If the attempt had FAILED
        // instead, call guard.unmark(&key) so the retry may run.
        TaskResult::Complete { variables }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let client = EngineClient::new("http://127.0.0.1:3000");
    let config = WorkerConfig::new("idempotent-worker-1").poll_interval(Duration::from_secs(1));

    let worker = Worker::builder()
        .client(client)
        .handler(IdempotentPaymentHandler {
            guard: tokio::sync::Mutex::new(IdempotencyGuard::new()),
        })
        .config(config)
        .build();

    tracing::info!("idempotent example worker; task_id is used as idempotency key");
    worker.start().await;
    Ok(())
}
