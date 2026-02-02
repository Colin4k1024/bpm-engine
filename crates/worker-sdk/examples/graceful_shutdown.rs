//! Graceful shutdown example: listen for SIGINT (Ctrl+C), then stop the worker after the current poll cycle.
//!
//! Run the Engine first: `cargo run -p bpm-engine-server-rest`
//! Then: `cargo run -p bpm-engine-worker-sdk --example graceful_shutdown`
//! Press Ctrl+C to shut down; the worker will finish the current iteration and exit.

use bpm_engine_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct ExampleHandler;

#[async_trait::async_trait]
impl TaskHandler for ExampleHandler {
    fn task_type(&self) -> &str {
        "payment"
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let amount = task.variables.get("amount").cloned().unwrap_or_else(|| "0".into());
        tracing::info!(task_id = %task.task_id, amount = %amount, "processing");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut variables = HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        TaskResult::Complete { variables }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let client = EngineClient::new("http://127.0.0.1:3000");
    let config = WorkerConfig::new("graceful-worker-1")
        .poll_interval(Duration::from_secs(1))
        .fetch_retry_max(3)
        .fetch_retry_backoff(Duration::from_secs(1));

    let worker = Worker::builder()
        .client(client)
        .handler(ExampleHandler)
        .config(config)
        .build();

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        shutdown_clone.store(true, Ordering::Relaxed);
        tracing::info!("shutdown signal received");
    });

    tracing::info!("worker starting; press Ctrl+C to stop gracefully");
    worker.start_until_signal(shutdown).await;
    tracing::info!("worker stopped");
    Ok(())
}
