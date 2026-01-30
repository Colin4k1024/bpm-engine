//! Worker configuration (design §5).

use std::time::Duration;

/// Worker runtime config: identity, poll, lock, concurrency.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub max_tasks: usize,
    pub lock_duration: Duration,
    pub poll_interval: Duration,
    pub concurrency_limit: Option<usize>,
}

impl WorkerConfig {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            max_tasks: 10,
            lock_duration: Duration::from_secs(30),
            poll_interval: Duration::from_secs(1),
            concurrency_limit: None,
        }
    }

    pub fn max_tasks(mut self, n: usize) -> Self {
        self.max_tasks = n;
        self
    }

    pub fn lock_duration(mut self, d: Duration) -> Self {
        self.lock_duration = d;
        self
    }

    pub fn poll_interval(mut self, d: Duration) -> Self {
        self.poll_interval = d;
        self
    }

    pub fn concurrency_limit(mut self, n: usize) -> Self {
        self.concurrency_limit = Some(n);
        self
    }
}
