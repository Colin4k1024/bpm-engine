//! Process instance and definition stores.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::collections::HashMap;
//! use bpm_engine_core::{ProcessInstance, InstanceState};
//! use bpm_engine_storage::ProcessInstanceStore;
//!
//! # async fn example(repo: Arc<impl ProcessInstanceStore>) -> anyhow::Result<()> {
//! // Save a process instance
//! let instance = ProcessInstance {
//!     id: "instance-1".into(),
//!     process_def_id: "approval-1".into(),
//!     tenant_id: None,
//!     tokens: vec![],
//!     variables: HashMap::new(),
//!     state: InstanceState::Running,
//!     version: 1,
//!     parent_instance_id: None,
//!     parent_token_id: None,
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
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use bpm_engine_core::{ProcessDefinition, ProcessInstance};
use serde::{Deserialize, Serialize};

/// Version status for process definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionStatus {
    /// Definition is active and can be used to start new instances.
    Active,
    /// Definition is deprecated; existing instances continue but no new instances.
    Deprecated,
}

impl std::fmt::Display for DefinitionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DefinitionStatus::Active => write!(f, "active"),
            DefinitionStatus::Deprecated => write!(f, "deprecated"),
        }
    }
}

/// Metadata record for a process definition version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessDefinitionRecord {
    /// Unique id for this version (e.g. "order-flow:3").
    pub id: String,
    /// Process definition key (e.g. "order-flow").
    pub key: String,
    /// Version number, monotonically increasing per key.
    pub version: u32,
    /// Current status: active or deprecated.
    pub status: DefinitionStatus,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
}

/// Stores BPMN process definitions (loaded from XML at deploy time).
///
/// Definitions are immutable after deploy; only the store id changes.
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

    /// List all versions of a process definition by key, ordered by version descending.
    async fn list_versions(&self, key: &str) -> anyhow::Result<Vec<ProcessDefinitionRecord>>;

    /// Get the currently active version of a process definition by key.
    async fn get_active(&self, key: &str) -> anyhow::Result<Option<ProcessDefinitionRecord>>;

    /// Set a specific version as the active one (deactivates previous active).
    async fn activate(&self, id: &str) -> anyhow::Result<()>;

    /// Mark a version as deprecated.
    async fn deprecate(&self, id: &str) -> anyhow::Result<()>;
}
