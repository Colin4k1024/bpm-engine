//! Payment worker example (design §6).
//!
//! Run the Engine first: `cargo run -p bpm-server-rest`
//! Then ensure the Engine has a process definition with an ExternalTask node (task_type = "payment").
//! Start this worker: `cargo run -p bpm-worker-sdk --example payment`

use async_trait::async_trait;
use bpm_engine_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};
use std::collections::HashMap;
use std::time::Duration;

struct PaymentHandler;

#[async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str {
        "payment"
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let amount = task
            .variables
            .get("amount")
            .cloned()
            .unwrap_or_else(|| "0".to_string());
        tracing::info!(task_id = %task.task_id, amount = %amount, "processing payment");

        // Simulate work
        tokio::time::sleep(Duration::from_millis(100)).await;

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
    let config = WorkerConfig::new("payment-worker-1")
        .max_tasks(5)
        .lock_duration(Duration::from_secs(30))
        .poll_interval(Duration::from_secs(1));

    let worker = Worker::builder()
        .client(client)
        .handler(PaymentHandler)
        .config(config)
        .build();

    tracing::info!("payment worker starting; poll Engine at http://127.0.0.1:3000");
    worker.start().await;
    Ok(())
}
