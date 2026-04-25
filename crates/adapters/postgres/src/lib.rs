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
pub mod event_store;
pub mod external_task_store;
pub mod history;
pub mod parallel_join;
pub mod process_store;
pub mod timer_store;
pub mod token_store;

pub use compensation::PostgresCompensationRepo;
pub use event_store::PostgresOutboxRepo;
pub use external_task_store::PostgresExternalTaskStore;
pub use history::PostgresHistoryRepo;
pub use parallel_join::PostgresParallelJoinRepo;
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

    // Create process_instance table
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS process_instance (
                id VARCHAR(255) PRIMARY KEY,
                process_def_id VARCHAR(255) NOT NULL,
                tenant_id VARCHAR(255),
                variables TEXT NOT NULL DEFAULT '{}',
                state VARCHAR(50) NOT NULL DEFAULT 'Running',
                version INTEGER NOT NULL DEFAULT 1,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
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
                id VARCHAR(255) NOT NULL,
                instance_id VARCHAR(255) NOT NULL,
                node_id VARCHAR(255) NOT NULL,
                status VARCHAR(50) NOT NULL DEFAULT 'Created',
                mode VARCHAR(50) NOT NULL DEFAULT 'Forward',
                version INTEGER NOT NULL DEFAULT 1,
                attempt INTEGER NOT NULL DEFAULT 0,
                parallel_group_id VARCHAR(255),
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
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
                id VARCHAR(255) PRIMARY KEY,
                token_id VARCHAR(255) NOT NULL,
                process_instance_id VARCHAR(255) NOT NULL,
                task_type VARCHAR(255) NOT NULL,
                retries INTEGER NOT NULL DEFAULT 3,
                timeout_secs BIGINT NOT NULL DEFAULT 300,
                variables TEXT NOT NULL DEFAULT '{}',
                state VARCHAR(50) NOT NULL DEFAULT 'Ready',
                worker_id VARCHAR(255),
                lock_expire_at TIMESTAMP,
                error_message TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
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
                id VARCHAR(255) PRIMARY KEY,
                token_id VARCHAR(255) NOT NULL,
                instance_id VARCHAR(255) NOT NULL,
                due_at VARCHAR(100) NOT NULL,
                status VARCHAR(50) NOT NULL DEFAULT 'Scheduled',
                created_at VARCHAR(100) NOT NULL,
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
                id VARCHAR(255) PRIMARY KEY,
                instance_id VARCHAR(255) NOT NULL,
                event_type VARCHAR(255) NOT NULL,
                payload TEXT NOT NULL,
                occurred_at VARCHAR(100) NOT NULL,
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
                id VARCHAR(255) PRIMARY KEY,
                tenant_id VARCHAR(255),
                event_type VARCHAR(255) NOT NULL,
                payload TEXT NOT NULL,
                status VARCHAR(50) NOT NULL DEFAULT 'Pending',
                claimed_by VARCHAR(255),
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
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
                id VARCHAR(255) PRIMARY KEY,
                instance_id VARCHAR(255) NOT NULL,
                node_id VARCHAR(255) NOT NULL,
                handler_ref VARCHAR(255) NOT NULL,
                "order" INTEGER NOT NULL DEFAULT 1,
                status VARCHAR(50) NOT NULL DEFAULT 'Pending',
                created_at VARCHAR(100) NOT NULL,
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
                id VARCHAR(255) PRIMARY KEY,
                parallel_group_id VARCHAR(255) NOT NULL UNIQUE,
                expected INTEGER NOT NULL,
                joined INTEGER NOT NULL DEFAULT 0
            )
            "#,
            &[],
        )
        .await?;

    // Create indexes for parallel_join
    client
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_parallel_join_group
            ON parallel_join(parallel_group_id)
            "#,
            &[],
        )
        .await?;

    Ok(())
}
