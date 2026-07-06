//! HTTP client for Engine external-task API (design §3).

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::types::ExternalTask;

/// Errors from the Engine client.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("network/HTTP error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("engine error (HTTP {status}): {message}")]
    Engine { status: u16, message: String },
}

/// Raw response from fetch-and-lock (matches Engine REST).
#[derive(Debug, Deserialize)]
struct FetchAndLockResponseItem {
    task_id: String,
    token_id: String,
    process_instance_id: String,
    task_type: String,
    variables: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
struct FetchAndLockRequest {
    worker_id: String,
    task_types: Vec<String>,
    max_tasks: u32,
    lock_duration_ms: u64,
}

#[derive(Debug, Serialize)]
struct CompleteRequest {
    worker_id: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    variables: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
struct FailRequest {
    worker_id: String,
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: String,
}

/// HTTP client for Engine external-task endpoints.
#[derive(Clone)]
pub struct EngineClient {
    client: reqwest::Client,
    base_url: String,
    tenant_id: Option<String>,
}

impl EngineClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            tenant_id: None,
        }
    }

    pub fn tenant_id(mut self, id: impl Into<String>) -> Self {
        self.tenant_id = Some(id.into());
        self
    }

    fn url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{}/api/v1/external-tasks{}", base, path)
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        if let Some(ref id) = self.tenant_id {
            h.insert(
                reqwest::header::HeaderName::from_static("x-tenant-id"),
                id.parse().unwrap(),
            );
        }
        h
    }

    /// Fetch and lock tasks from the Engine.
    pub async fn fetch_and_lock(
        &self,
        worker_id: &str,
        task_types: &[String],
        max_tasks: usize,
        lock_duration_ms: u64,
    ) -> Result<Vec<ExternalTask>, ClientError> {
        let url = self.url("/fetch-and-lock");
        let body = FetchAndLockRequest {
            worker_id: worker_id.to_string(),
            task_types: task_types.to_vec(),
            max_tasks: max_tasks as u32,
            lock_duration_ms,
        };
        debug!(%url, "fetch_and_lock");
        let req = self.client.post(&url).json(&body).headers(self.headers());
        let res = req.send().await?;
        let status = res.status();
        if !status.is_success() {
            let message = res
                .json::<ErrorBody>()
                .await
                .map(|b| b.error)
                .unwrap_or_else(|_| status.to_string());
            return Err(ClientError::Engine {
                status: status.as_u16(),
                message,
            });
        }
        let items: Vec<FetchAndLockResponseItem> = res.json().await?;
        let tasks = items
            .into_iter()
            .map(|i| ExternalTask {
                task_id: i.task_id,
                task_type: i.task_type,
                variables: i.variables,
                lock_expire_at: None,
                retries: 0,
                #[cfg(debug_assertions)]
                token_id: Some(i.token_id),
                #[cfg(debug_assertions)]
                process_instance_id: Some(i.process_instance_id),
            })
            .collect();
        Ok(tasks)
    }

    /// Complete a task.
    pub async fn complete(
        &self,
        task_id: &str,
        worker_id: &str,
        variables: HashMap<String, String>,
    ) -> Result<(), ClientError> {
        let url = self.url(&format!("/{}/complete", task_id));
        let body = CompleteRequest {
            worker_id: worker_id.to_string(),
            variables,
        };
        debug!(%url, "complete");
        let res = self
            .client
            .post(&url)
            .json(&body)
            .headers(self.headers())
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
            let message = res
                .json::<ErrorBody>()
                .await
                .map(|b| b.error)
                .unwrap_or_else(|_| status.to_string());
            return Err(ClientError::Engine {
                status: status.as_u16(),
                message,
            });
        }
        Ok(())
    }

    /// Fail a task.
    pub async fn fail(
        &self,
        task_id: &str,
        worker_id: &str,
        error: String,
        retry_after: Option<Duration>,
    ) -> Result<(), ClientError> {
        let url = self.url(&format!("/{}/fail", task_id));
        let retry_after_ms = retry_after.map(|d| d.as_millis() as u64);
        let body = FailRequest {
            worker_id: worker_id.to_string(),
            error,
            retry_after_ms,
        };
        debug!(%url, "fail");
        let res = self
            .client
            .post(&url)
            .json(&body)
            .headers(self.headers())
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
            let message = res
                .json::<ErrorBody>()
                .await
                .map(|b| b.error)
                .unwrap_or_else(|_| status.to_string());
            return Err(ClientError::Engine {
                status: status.as_u16(),
                message,
            });
        }
        Ok(())
    }

    /// Extend the lock on a locked task to prevent timeout during long processing.
    pub async fn extend_lock(
        &self,
        task_id: &str,
        worker_id: &str,
        extension: Duration,
    ) -> Result<(), ClientError> {
        #[derive(Serialize)]
        struct ExtendLockBody {
            worker_id: String,
            extension_ms: u64,
        }
        let url = self.url(&format!("/{}/extend-lock", task_id));
        let body = ExtendLockBody {
            worker_id: worker_id.to_string(),
            extension_ms: extension.as_millis() as u64,
        };
        debug!(%url, "extend_lock");
        let res = self
            .client
            .post(&url)
            .json(&body)
            .headers(self.headers())
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
            let message = res
                .json::<ErrorBody>()
                .await
                .map(|b| b.error)
                .unwrap_or_else(|_| status.to_string());
            return Err(ClientError::Engine {
                status: status.as_u16(),
                message,
            });
        }
        Ok(())
    }
}
