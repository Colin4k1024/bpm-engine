//! Worker runtime: poll loop and task spawning (design §3, §4).

use std::collections::HashMap;
use std::sync::Arc;

use std::time::Instant;
use tracing::{info, warn};

use crate::client::EngineClient;
use crate::config::WorkerConfig;
use crate::handler::{TaskContext, TaskHandler};
use crate::types::{ExternalTask, TaskResult};

/// Worker: poll Engine for tasks and run handlers (design §3).
pub struct Worker {
    client: EngineClient,
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
    config: WorkerConfig,
}

#[derive(Default)]
pub struct WorkerBuilder {
    client: Option<EngineClient>,
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
    config: Option<WorkerConfig>,
}

impl WorkerBuilder {
    pub fn client(mut self, client: EngineClient) -> Self {
        self.client = Some(client);
        self
    }

    pub fn handler(mut self, h: impl TaskHandler + 'static) -> Self {
        let t = h.task_type().to_string();
        self.handlers.insert(t, Arc::new(h));
        self
    }

    pub fn config(mut self, config: WorkerConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn build(self) -> Worker {
        Worker {
            client: self.client.expect("EngineClient required"),
            handlers: self.handlers,
            config: self.config.expect("WorkerConfig required"),
        }
    }
}

impl Worker {
    pub fn builder() -> WorkerBuilder {
        WorkerBuilder::default()
    }

    /// Run the poll loop until cancelled (design §3).
    pub async fn start(&self) {
        let task_types: Vec<String> = self.handlers.keys().cloned().collect();
        if task_types.is_empty() {
            warn!("no handlers registered; worker will not fetch any tasks");
        }
        let worker_id = self.config.worker_id.clone();
        let max_tasks = self.config.max_tasks;
        let lock_duration_ms = self.config.lock_duration.as_millis() as u64;
        let poll_interval = self.config.poll_interval;

        loop {
            if task_types.is_empty() {
                tokio::time::sleep(poll_interval).await;
                continue;
            }
            match self
                .client
                .fetch_and_lock(&worker_id, &task_types, max_tasks, lock_duration_ms)
                .await
            {
                Ok(tasks) => {
                    for task in tasks {
                        if self.handlers.contains_key(&task.task_type) {
                            self.spawn_task(task);
                        } else {
                            warn!(task_type = %task.task_type, "no handler for task type");
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "fetch_and_lock failed");
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Spawn one task; on handler panic, call fail (design §4, §6).
    fn spawn_task(&self, task: ExternalTask) {
        let handler = match self.handlers.get(&task.task_type) {
            Some(h) => Arc::clone(h),
            None => return,
        };
        let client = self.client.clone();
        let worker_id = self.config.worker_id.clone();
        let task_id = task.task_id.clone();
        let ctx = TaskContext::new(worker_id.clone(), task.task_id.clone());

        let task_type = task.task_type.clone();
        tokio::spawn(async move {
            let start = Instant::now();
            let join = tokio::spawn(async move { handler.handle(task, ctx).await });
            let duration_ms = move || start.elapsed().as_millis();
            match join.await {
                Ok(TaskResult::Complete { variables }) => {
                    info!(
                        task_id = %task_id,
                        task_type = %task_type,
                        worker_id = %worker_id,
                        duration_ms = duration_ms(),
                        result = "Complete",
                        "task finished"
                    );
                    if let Err(e) = client.complete(&task_id, &worker_id, variables).await {
                        tracing::warn!(task_id = %task_id, error = %e, "complete failed");
                    }
                }
                Ok(TaskResult::Fail { error, retry_after }) => {
                    info!(
                        task_id = %task_id,
                        task_type = %task_type,
                        worker_id = %worker_id,
                        duration_ms = duration_ms(),
                        result = "Fail",
                        error = %error,
                        "task failed"
                    );
                    if let Err(e) = client.fail(&task_id, &worker_id, error, retry_after).await {
                        tracing::warn!(task_id = %task_id, error = %e, "fail failed");
                    }
                }
                Err(join_err) => {
                    let msg = if join_err.is_panic() {
                        "handler panic".to_string()
                    } else {
                        "handler task cancelled".to_string()
                    };
                    info!(
                        task_id = %task_id,
                        task_type = %task_type,
                        worker_id = %worker_id,
                        duration_ms = duration_ms(),
                        result = "Panic",
                        error = %msg,
                        "task panic/cancelled"
                    );
                    let _ = client.fail(&task_id, &worker_id, msg, None).await;
                }
            }
        });
    }
}
