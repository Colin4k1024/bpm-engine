//! Process instance and definition stores.
//!
//! # Example
//!
//! ```ignore
//! let repo = Arc::new(MemoryRepo::new());
//!
//! // Save a process instance
//! let instance = ProcessInstance {
//!     id: "instance-1".into(),
//!     process_def_id: "approval-1".into(),
//!     state: InstanceState::Running,
//!     version: 1,
//! };
//! repo.save(&instance).await?;
//!
//! // Load by id
//! let loaded = repo.load("instance-1").await?;
//! assert!(loaded.is_some());
//!
//! // List running instances (None = all tenants)
//! let running = repo.list_running(None).await?;
//! assert!(running.contains(&"instance-1".into()));
//! ```

use async_trait::async_trait;
use bpm_engine_core::{ProcessDefinition, ProcessInstance};

/// Stores active process instances and supports tenant-scoped queries.
///
/// Process instances are created when a BPMN process is started
/// and track the current state of a running (or suspended/completed/terminated) process.
#[async_trait]
pub trait ProcessInstanceStore: Send + Sync {
    /// Load a process instance by its unique id.
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessInstance>>;

    /// Persist an instance (upsert — insert or replace).
    async fn save(&self, instance: &ProcessInstance) -> anyhow::Result<()>;

    /// List ids of all running instances, optionally filtered by tenant.
    async fn list_running(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>>;
}

/// Stores BPMN process definitions (loaded from XML at deploy time).
///
/// Definitions are immutable after deploy; only the store id changes.
#[async_trait]
pub trait ProcessDefinitionStore: Send + Sync {
    /// Load a process definition by its unique id.
    async fn load(&self, id: &str) -> anyhow::Result<Option<ProcessDefinition>>;
}
