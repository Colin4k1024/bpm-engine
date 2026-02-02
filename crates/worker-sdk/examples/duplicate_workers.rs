//! Duplicate workers example: run two workers in parallel; each task is locked by only one worker.
//!
//! The engine's fetch-and-lock ensures that a task is given to at most one worker at a time.
//! Run two processes (or two tasks) with different worker_id; they will share the same task types
//! and the engine will distribute work. If one worker crashes, the lock expires and the task
//! becomes available again (reclaim).
//!
//! Run the Engine first: `cargo run -p bpm-engine-server-rest`
//! Start a process: `cargo run -p bpm-engine-worker-sdk --example duplicate_workers`
//! (Optional) Start a second process with the same command to see two workers polling; only one
//! will get each task.

use bpm_engine_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};
use std::collections::HashMap;
use std::time::Duration;

struct PaymentHandler {
    worker_name: String,
}

#[async_trait::async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str {
        "payment"
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        tracing::info!(
            task_id = %task.task_id,
            worker_name = %self.worker_name,
            "processing (only one worker owns this task)"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut variables = HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        variables.insert("processed_by".to_string(), self.worker_name.clone());
        TaskResult::Complete { variables }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let worker_id = std::env::var("WORKER_ID").unwrap_or_else(|_| {
        format!("worker-{}", std::process::id())
    });
    let client = EngineClient::new("http://127.0.0.1:3000");
    let config = WorkerConfig::new(&worker_id)
        .poll_interval(Duration::from_secs(1))
        .max_tasks(5);

    let worker = Worker::builder()
        .client(client)
        .handler(PaymentHandler {
            worker_name: worker_id.clone(),
        })
        .config(config)
        .build();

    tracing::info!(worker_id = %worker_id, "duplicate worker starting; start another process to see lock uniqueness");
    worker.start().await;
    Ok(())
}
