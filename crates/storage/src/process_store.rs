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

    /// List ids of all instances regardless of state, optionally filtered by tenant.
    ///
    /// Unlike [`ProcessInstanceStore::list_running`], this includes completed
    /// and terminated instances. Implementations must return ids in a
    /// deterministic order (sorted by instance id).
    async fn list_all(&self, tenant_id: Option<&str>) -> anyhow::Result<Vec<String>>;

    /// List one page of instance ids (all states), in the same deterministic
    /// order as [`ProcessInstanceStore::list_all`] (sorted by instance id).
    ///
    /// Returns at most `limit` ids starting at `offset` (an offset beyond the
    /// end returns an empty vec). Backends with SQL support push the window
    /// down (`LIMIT`/`OFFSET`); this is the hot listing path for the
    /// paginated `GET /api/v1/process-instances` endpoint (E2-4).
    async fn list_page(
        &self,
        tenant_id: Option<&str>,
        offset: u64,
        limit: u64,
    ) -> anyhow::Result<Vec<String>>;

    /// Look up the instance id previously recorded for a start-instance
    /// idempotency key (E2-4). `Ok(None)` when the key is unknown for the
    /// given tenant scope.
    async fn find_idempotency_key(
        &self,
        key: &str,
        tenant_id: Option<&str>,
    ) -> anyhow::Result<Option<String>>;

    /// Record (upsert) the instance id for a start-instance idempotency key
    /// (E2-4). The key is scoped by tenant: the same key under different
    /// tenants maps to different instances.
    async fn save_idempotency_key(
        &self,
        key: &str,
        tenant_id: Option<&str>,
        instance_id: &str,
    ) -> anyhow::Result<()>;
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

    /// List every version of every process definition key.
    ///
    /// Ordered by key ascending, then version descending. Used by management
    /// APIs that need a full inventory of deployed definitions.
    async fn list_all(&self) -> anyhow::Result<Vec<ProcessDefinitionRecord>>;
}

/// Persists a process definition version at deploy time.
///
/// `def` is the already-compiled definition (used for validation/diagnostics
/// by the caller); `bpmn_xml` is the source that backends persist so the
/// definition can be recompiled after a restart.
#[async_trait]
pub trait DefinitionDeployer: Send + Sync {
    /// Persist a definition version. `def` is the already-compiled definition
    /// (used for validation/diagnostics by the caller); `bpmn_xml` is the source.
    async fn deploy(&self, def: &ProcessDefinition, bpmn_xml: &str) -> anyhow::Result<()>;
}
