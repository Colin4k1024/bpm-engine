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

pub mod process_store;
pub mod token_store;

pub use process_store::PostgresProcessStore;
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

    Ok(())
}
