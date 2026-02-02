//! Idempotency example: use task_id (or a business key from variables) to avoid double-processing.
//!
//! If the worker crashes after doing work but before calling complete(), the engine will
//! eventually reclaim the lock and another worker may receive the same task. Use an idempotency
//! key (e.g. task_id or instance_id + node_id) to check "already processed?" before doing work.
//!
//! Run the Engine first: `cargo run -p bpm-engine-server-rest`
//! Then: `cargo run -p bpm-engine-worker-sdk --example idempotency`

use bpm_engine_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// In-memory set of completed task ids (in production, use a DB or cache like Redis).
static PROCESSED: Mutex<Option<HashMap<String, ()>>> = Mutex::new(None);

fn mark_done(id: &str) {
    let mut g = PROCESSED.lock().unwrap();
    if g.is_none() {
        *g = Some(HashMap::new());
    }
    g.as_mut().unwrap().insert(id.to_string(), ());
}

fn already_done(id: &str) -> bool {
    let g = PROCESSED.lock().unwrap();
    g.as_ref().map_or(false, |m| m.contains_key(id))
}

struct IdempotentPaymentHandler;

#[async_trait::async_trait]
impl TaskHandler for IdempotentPaymentHandler {
    fn task_type(&self) -> &str {
        "payment"
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let idempotency_key = &task.task_id;
        if already_done(idempotency_key) {
            tracing::info!(task_id = %task.task_id, "already processed (idempotency), completing");
            let mut variables = HashMap::new();
            variables.insert("status".to_string(), "PAID".to_string());
            return TaskResult::Complete { variables };
        }

        let amount = task.variables.get("amount").cloned().unwrap_or_else(|| "0".into());
        tracing::info!(task_id = %task.task_id, amount = %amount, "processing payment");
        tokio::time::sleep(Duration::from_millis(100)).await;

        mark_done(idempotency_key);
        let mut variables = HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        variables.insert("amount".to_string(), amount);
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
        .handler(IdempotentPaymentHandler)
        .config(config)
        .build();

    tracing::info!("idempotency example worker; task_id is used as idempotency key");
    worker.start().await;
    Ok(())
}
