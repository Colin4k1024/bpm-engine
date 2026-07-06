//! PostgreSQL adapter for the BPM engine.
//!
//! This adapter provides PostgreSQL-backed implementations of the storage traits
//! defined in `bpm-engine-storage`. It uses `tokio-postgres` and `deadpool-postgres`
//! for async database operations with connection pooling.
//!
//! # Example
//!
//! ```ignore
//! use bpm_engine_adapter_postgres::{PostgresTokenStore, migrate, create_pool};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let pool = create_pool("postgres://user:pass@localhost/bpm")?;
//!
//!     migrate(&pool).await?;
//!
//!     let token_store = PostgresTokenStore::new(pool);
//!     // Use with BpmEngine...
//!     Ok(())
//! }
//! ```

pub mod compensation;
pub mod dead_letter_store;
pub mod event_store;
pub mod external_task_store;
pub mod history;
pub mod parallel_join;
pub mod pool_metrics;
pub mod process_def_store;
pub mod process_store;
pub mod timer_store;
pub mod token_store;

pub use compensation::PostgresCompensationRepo;
pub use dead_letter_store::PostgresDeadLetterStore;
pub use event_store::PostgresOutboxRepo;
pub use external_task_store::PostgresExternalTaskStore;
pub use history::PostgresHistoryRepo;
pub use parallel_join::PostgresParallelJoinRepo;
pub use process_def_store::PostgresProcessDefStore;
pub use process_store::PostgresProcessStore;
pub use timer_store::PostgresTimerStore;
pub use token_store::PostgresTokenStore;

use deadpool_postgres::Pool;
use tokio_postgres::NoTls;
use url::Url;

/// Create a PostgreSQL connection pool.
///
/// # Arguments
///
/// * `url` - PostgreSQL connection string (e.g., `postgres://user:pass@localhost/bpm`)
///
/// # Example
///
/// ```ignore
/// let pool = create_pool("postgres://user:pass@localhost/bpm")?;
/// ```
pub fn create_pool(url: &str) -> anyhow::Result<Pool> {
    let url = Url::parse(url)?;
    let mut cfg = deadpool_postgres::Config::new();
    cfg.host = Some(url.host_str().unwrap_or("localhost").to_string());
    cfg.port = Some(url.port().unwrap_or(5432));
    let user = url.username();
    if !user.is_empty() {
        cfg.user = Some(user.to_string());
        if let Some(pass) = url.password() {
            cfg.password = Some(pass.to_string());
        }
    }
    cfg.dbname = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .map(|s| s.to_string());

    let pool = cfg.create_pool(Some(deadpool_postgres::Runtime::Tokio1), NoTls)?;

    Ok(pool)
}

/// Run database migrations to create the required tables.
///
/// This function creates the following tables if they don't exist:
/// - `process_instance` - Stores process instances
/// - `token` - Stores execution tokens
/// - `external_task` - Stores external task definitions and state
/// - `timer` - Stores persistent timer records
/// - `history_event` - Stores execution history events
/// - `event_outbox` - Transactional outbox for event publishing
/// - `compensation_record` - Saga compensation records
/// - `parallel_join` - Fork/join coordination
///
/// # Arguments
///
/// * `pool` - PostgreSQL connection pool
///
/// # Example
///
/// ```ignore
/// let pool = create_pool("postgres://user:pass@localhost/bpm")?;
/// migrate(&pool).await?;
/// ```
pub async fn migrate(pool: &Pool) -> anyhow::Result<()> {
    let client = pool.get().await?;

    // Create process_definition table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS process_definition (
                id TEXT PRIMARY KEY,
                def_key TEXT NOT NULL DEFAULT '',
                version INTEGER NOT NULL DEFAULT 1,
                status TEXT NOT NULL DEFAULT 'active',
                bpmn_xml TEXT NOT NULL,
                created_at TEXT
            )
            "#,
            &[],
        )
        .await?;

    // Create process_instance table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS process_instance (
                id TEXT PRIMARY KEY,
                process_def_id TEXT NOT NULL,
                tenant_id TEXT,
                variables TEXT NOT NULL DEFAULT '{}',
                state TEXT NOT NULL DEFAULT 'Running',
                version INTEGER NOT NULL DEFAULT 1,
                parent_instance_id TEXT,
                parent_token_id TEXT,
                created_at TEXT,
                updated_at TEXT
            )
            "#,
            &[],
        )
        .await?;

    // Create token table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS token (
                id TEXT NOT NULL,
                instance_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'Created',
                mode TEXT NOT NULL DEFAULT 'Forward',
                version INTEGER NOT NULL DEFAULT 1,
                attempt INTEGER NOT NULL DEFAULT 0,
                parallel_group_id TEXT,
                created_at TEXT,
                updated_at TEXT,
                PRIMARY KEY (id, instance_id),
                CONSTRAINT fk_token_instance FOREIGN KEY (instance_id)
                    REFERENCES process_instance(id) ON DELETE CASCADE
            )
            "#,
            &[],
        )
        .await?;

    // Create indexes for token table
    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_token_state_updated
            ON token(state, updated_at)
            "#,
            &[],
        )
        .await?;

    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_token_parallel_group
            ON token(parallel_group_id)
            "#,
            &[],
        )
        .await?;

    // Create external_task table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS external_task (
                id TEXT PRIMARY KEY,
                token_id TEXT NOT NULL,
                process_instance_id TEXT NOT NULL,
                task_type TEXT NOT NULL,
                retries INTEGER NOT NULL DEFAULT 3,
                timeout_secs INTEGER NOT NULL DEFAULT 300,
                variables TEXT NOT NULL DEFAULT '{}',
                state TEXT NOT NULL DEFAULT 'Ready',
                worker_id TEXT,
                lock_expire_at TEXT,
                error_message TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT,
                updated_at TEXT,
                CONSTRAINT fk_external_task_instance FOREIGN KEY (process_instance_id)
                    REFERENCES process_instance(id) ON DELETE CASCADE
            )
            "#,
            &[],
        )
        .await?;

    // Create indexes for external_task
    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_external_task_state
            ON external_task(state, lock_expire_at)
            "#,
            &[],
        )
        .await?;

    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_external_task_type
            ON external_task(task_type)
            "#,
            &[],
        )
        .await?;

    // Create timer table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS timer (
                id TEXT PRIMARY KEY,
                token_id TEXT NOT NULL,
                instance_id TEXT NOT NULL,
                node_id TEXT NOT NULL DEFAULT '',
                due_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'Scheduled',
                created_at TEXT NOT NULL,
                CONSTRAINT fk_timer_instance FOREIGN KEY (instance_id)
                    REFERENCES process_instance(id) ON DELETE CASCADE
            )
            "#,
            &[],
        )
        .await?;

    // Create indexes for timer
    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_timer_due
            ON timer(status, due_at)
            "#,
            &[],
        )
        .await?;

    // Create history_event table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS history_event (
                id TEXT PRIMARY KEY,
                instance_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                occurred_at TEXT NOT NULL,
                CONSTRAINT fk_history_instance FOREIGN KEY (instance_id)
                    REFERENCES process_instance(id) ON DELETE CASCADE
            )
            "#,
            &[],
        )
        .await?;

    // Create indexes for history_event
    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_history_instance
            ON history_event(instance_id, occurred_at)
            "#,
            &[],
        )
        .await?;

    // Create event_outbox table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS event_outbox (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'Pending',
                created_at TEXT
            )
            "#,
            &[],
        )
        .await?;

    // Create indexes for event_outbox
    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_outbox_status
            ON event_outbox(status, created_at)
            "#,
            &[],
        )
        .await?;

    // Create compensation_record table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS compensation_record (
                id TEXT PRIMARY KEY,
                instance_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                handler_ref TEXT,
                "order" INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'Pending',
                created_at TEXT,
                CONSTRAINT fk_compensation_instance FOREIGN KEY (instance_id)
                    REFERENCES process_instance(id) ON DELETE CASCADE
            )
            "#,
            &[],
        )
        .await?;

    // Create indexes for compensation_record
    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_compensation_instance
            ON compensation_record(instance_id, "order")
            "#,
            &[],
        )
        .await?;

    // Create parallel_join table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS parallel_join (
                parallel_group_id TEXT PRIMARY KEY,
                expected INTEGER NOT NULL,
                joined INTEGER NOT NULL DEFAULT 0
            )
            "#,
            &[],
        )
        .await?;

    // Create dead_letter table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS dead_letter (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                token_id TEXT NOT NULL,
                process_instance_id TEXT NOT NULL,
                task_type TEXT NOT NULL,
                error_message TEXT NOT NULL DEFAULT '',
                variables TEXT NOT NULL DEFAULT '{}',
                tenant_id TEXT,
                created_at TEXT NOT NULL,
                CONSTRAINT fk_dead_letter_instance FOREIGN KEY (process_instance_id)
                    REFERENCES process_instance(id) ON DELETE CASCADE
            )
            "#,
            &[],
        )
        .await?;

    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_dead_letter_instance
            ON dead_letter(process_instance_id)
            "#,
            &[],
        )
        .await?;

    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_dead_letter_type
            ON dead_letter(task_type)
            "#,
            &[],
        )
        .await?;

    Ok(())
}
