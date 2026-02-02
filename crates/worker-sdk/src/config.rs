//! Worker configuration (design §5).

use std::time::Duration;

/// Worker runtime config: identity, poll, lock, concurrency, fetch retry/backoff.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub max_tasks: usize,
    pub lock_duration: Duration,
    pub poll_interval: Duration,
    pub concurrency_limit: Option<usize>,
    /// Max retries for fetch_and_lock on transport/engine error (default 5).
    pub fetch_retry_max: usize,
    /// Initial backoff duration for fetch retries; doubles each retry, capped at 30s (default 1s).
    pub fetch_retry_backoff: Duration,
}

impl WorkerConfig {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
            max_tasks: 10,
            lock_duration: Duration::from_secs(30),
            poll_interval: Duration::from_secs(1),
            concurrency_limit: None,
            fetch_retry_max: 5,
            fetch_retry_backoff: Duration::from_secs(1),
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

    /// Set max retries for fetch_and_lock (exponential backoff). Default 5.
    pub fn fetch_retry_max(mut self, n: usize) -> Self {
        self.fetch_retry_max = n;
        self
    }

    /// Set initial backoff for fetch retries. Default 1s.
    pub fn fetch_retry_backoff(mut self, d: Duration) -> Self {
        self.fetch_retry_backoff = d;
        self
    }
}
