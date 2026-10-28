//! Storage assembly: bundles every store trait object the REST server needs.
//!
//! [`Storage::memory`] composes the in-memory adapters (default backend).
//! [`Storage::postgres`] (feature `postgres`) creates a connection pool, runs
//! migrations (idempotent — safe on every start), and wires all Postgres-backed
//! stores over the shared pool, plus a readiness-check callback for `/ready`.
//!
//! REST routes and the engine only see the trait objects in this bundle, so
//! switching backends is a pure configuration change (`database.driver`).

#[cfg(feature = "postgres")]
use crate::state::ExtraHealthChecks;
use bpm_engine_adapter_memory::{DeadLetterRepo, MemoryRepo, ProcessDefStore};
use bpm_engine_storage::{
    CompensationRecordRepo, DeadLetterStore, DefinitionDeployer, ExternalTaskStore, HistoryRepo,
    InvariantChecker, ParallelJoinRepo, ProcessDefinitionStore, ProcessInstanceStore,
    StoreInvariantChecker, TimerStore, TokenStore,
};
use std::sync::Arc;

/// Bundle of all storage trait objects used by the REST server.
///
/// Construct via [`Storage::memory`] or [`Storage::postgres`]. Every field is
/// a cloneable trait object; the concrete backend is hidden from callers.
pub struct Storage {
    /// Process instance persistence.
    pub process_store: Arc<dyn ProcessInstanceStore>,
    /// Token persistence with optimistic concurrency.
    pub token_store: Arc<dyn TokenStore>,
    /// Process definition storage (versions + compiled definitions).
    pub def_store: Arc<dyn ProcessDefinitionStore>,
    /// Definition deploy API (persists BPMN XML source).
    pub deployer: Arc<dyn DefinitionDeployer>,
    /// Persistent timer storage.
    pub timer_store: Arc<dyn TimerStore>,
    /// Parallel join coordination.
    pub parallel_join_repo: Arc<dyn ParallelJoinRepo>,
    /// Saga compensation record storage.
    pub compensation_repo: Arc<dyn CompensationRecordRepo>,
    /// External task storage.
    pub external_task_store: Arc<dyn ExternalTaskStore>,
    /// Execution history recording.
    pub history_repo: Arc<dyn HistoryRepo>,
    /// Dead letter queue storage.
    pub dead_letter_store: Arc<dyn DeadLetterStore>,
    /// Invariant checker over the process instance store.
    pub invariant_checker: Arc<dyn InvariantChecker>,
}

impl Storage {
    /// Build the memory-backed storage bundle with fresh stores.
    pub fn memory() -> Self {
        Self::memory_with_def_store(Arc::new(ProcessDefStore::new()))
    }

    /// Build the memory-backed storage bundle over the given definition store.
    ///
    /// The caller may pre-register definitions (e.g. bootstrap definitions)
    /// before handing the store over.
    pub fn memory_with_def_store(def_store: Arc<ProcessDefStore>) -> Self {
        let repo = Arc::new(MemoryRepo::new());
        Storage {
            process_store: Arc::clone(&repo) as Arc<dyn ProcessInstanceStore>,
            token_store: Arc::clone(&repo) as Arc<dyn TokenStore>,
            def_store: Arc::clone(&def_store) as Arc<dyn ProcessDefinitionStore>,
            deployer: Arc::clone(&def_store) as Arc<dyn DefinitionDeployer>,
            timer_store: Arc::clone(&repo) as Arc<dyn TimerStore>,
            parallel_join_repo: Arc::clone(&repo) as Arc<dyn ParallelJoinRepo>,
            compensation_repo: Arc::clone(&repo) as Arc<dyn CompensationRecordRepo>,
            external_task_store: Arc::clone(&repo) as Arc<dyn ExternalTaskStore>,
            history_repo: Arc::clone(&repo) as Arc<dyn HistoryRepo>,
            dead_letter_store: Arc::new(DeadLetterRepo::new()) as Arc<dyn DeadLetterStore>,
            invariant_checker: Arc::new(StoreInvariantChecker::new(
                Arc::clone(&repo) as Arc<dyn ProcessInstanceStore>
            )),
        }
    }

    /// Build the Postgres-backed storage bundle.
    ///
    /// Creates a connection pool (max `max_connections` connections), runs
    /// migrations (idempotent), and wires all stores over the shared pool.
    /// Returns a readiness-check callback that reports pool status for the
    /// `/ready` endpoint.
    #[cfg(feature = "postgres")]
    pub async fn postgres(
        url: &str,
        max_connections: u32,
    ) -> anyhow::Result<(Storage, ExtraHealthChecks)> {
        use bpm_engine_adapter_postgres::{
            PostgresCompensationRepo, PostgresDeadLetterStore, PostgresExternalTaskStore,
            PostgresHistoryRepo, PostgresParallelJoinRepo, PostgresProcessDefStore,
            PostgresProcessStore, PostgresTimerStore, PostgresTokenStore,
        };

        let pool = bpm_engine_adapter_postgres::create_pool_with_max_size(url, max_connections)?;
        bpm_engine_adapter_postgres::migrate(&pool).await?;

        let process_store = Arc::new(PostgresProcessStore::new(pool.clone()));
        let def_store = Arc::new(PostgresProcessDefStore::new(pool.clone()));
        let storage = Storage {
            process_store: Arc::clone(&process_store) as Arc<dyn ProcessInstanceStore>,
            token_store: Arc::new(PostgresTokenStore::new(pool.clone())),
            def_store: Arc::clone(&def_store) as Arc<dyn ProcessDefinitionStore>,
            deployer: Arc::clone(&def_store) as Arc<dyn DefinitionDeployer>,
            timer_store: Arc::new(PostgresTimerStore::new(pool.clone())),
            parallel_join_repo: Arc::new(PostgresParallelJoinRepo::new(pool.clone())),
            compensation_repo: Arc::new(PostgresCompensationRepo::new(pool.clone())),
            external_task_store: Arc::new(PostgresExternalTaskStore::new(pool.clone())),
            history_repo: Arc::new(PostgresHistoryRepo::new(pool.clone())),
            dead_letter_store: Arc::new(PostgresDeadLetterStore::new(pool.clone())),
            invariant_checker: Arc::new(StoreInvariantChecker::new(
                process_store as Arc<dyn ProcessInstanceStore>,
            )),
        };

        // Readiness probe: report deadpool status under the db_pool* keys.
        let pool_for_checks = pool.clone();
        let extra_health_checks: ExtraHealthChecks = Arc::new(move || {
            bpm_engine_adapter_postgres::pool_metrics::readiness_checks(&pool_for_checks)
        });

        Ok((storage, extra_health_checks))
    }
}
