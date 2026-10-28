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
//!
//! # Migrations
//!
//! The schema is managed by versioned, forward-only SQL migrations (see
//! [`migrations`]). [`migrate`] is a backward-compatible thin wrapper around
//! [`migrations::run_migrations`].

pub mod compensation;
pub mod dead_letter_store;
pub mod event_store;
pub mod external_task_store;
pub mod history;
pub mod migrations;
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
pub use migrations::{migration_status, run_migrations, validate_up_to_date, MigrationStatus};
pub use parallel_join::PostgresParallelJoinRepo;
pub use process_def_store::PostgresProcessDefStore;
pub use process_store::PostgresProcessStore;
pub use timer_store::PostgresTimerStore;
pub use token_store::PostgresTokenStore;

use deadpool_postgres::Pool;
use tokio_postgres::NoTls;
use url::Url;

/// Parse a PostgreSQL connection URL into a deadpool configuration.
fn pool_config_from_url(url: &str) -> anyhow::Result<deadpool_postgres::Config> {
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
    Ok(cfg)
}

/// Create a PostgreSQL connection pool with deadpool's default max size.
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
    let cfg = pool_config_from_url(url)?;
    let pool = cfg.create_pool(Some(deadpool_postgres::Runtime::Tokio1), NoTls)?;
    Ok(pool)
}

/// Create a PostgreSQL connection pool with an explicit maximum pool size.
///
/// # Arguments
///
/// * `url` - PostgreSQL connection string (e.g., `postgres://user:pass@localhost/bpm`)
/// * `max_size` - Maximum number of connections in the pool
///
/// # Example
///
/// ```ignore
/// let pool = create_pool_with_max_size("postgres://user:pass@localhost/bpm", 5)?;
/// ```
pub fn create_pool_with_max_size(url: &str, max_size: u32) -> anyhow::Result<Pool> {
    let mut cfg = pool_config_from_url(url)?;
    cfg.pool = Some(deadpool_postgres::PoolConfig::new(max_size as usize));
    let pool = cfg.create_pool(Some(deadpool_postgres::Runtime::Tokio1), NoTls)?;
    Ok(pool)
}

/// Run database migrations (backward-compatible wrapper).
///
/// Equivalent to [`migrations::run_migrations`]: applies every pending
/// versioned migration (each in its own transaction), with baseline stamping
/// for pre-versioning databases. Idempotent — safe on every start; a no-op
/// when the schema is already up to date. See the [`migrations`] module for
/// the migration list, [`migrations::migration_status`] for introspection,
/// and ADR-005 for the forward-only strategy.
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
    crate::migrations::run_migrations(pool).await?;
    Ok(())
}
