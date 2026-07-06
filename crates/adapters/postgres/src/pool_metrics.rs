//! Connection pool monitoring and metrics.
//!
//! Exposes `deadpool-postgres` pool status as structured data that can be
//! consumed by health checks, Prometheus, and OpenTelemetry.

use deadpool_postgres::Pool;

/// Snapshot of connection pool state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PoolStatus {
    /// Number of connections currently in use.
    pub active: usize,
    /// Number of idle connections in the pool.
    pub idle: usize,
    /// Maximum number of connections allowed.
    pub max_size: usize,
    /// Number of requests currently waiting to acquire a connection.
    pub wait_count: usize,
}

/// Retrieve the current pool status.
///
/// This is a lightweight call — `deadpool` tracks pool state atomically.
pub fn pool_status(pool: &Pool) -> PoolStatus {
    let status = pool.status();
    PoolStatus {
        active: status.size - status.available,
        idle: status.available,
        max_size: status.max_size,
        wait_count: status.waiting,
    }
}

/// Log a structured alert when connection acquisition fails.
///
/// Call this in the `Err` branch of `pool.get().await`.
pub fn log_connection_failure(error: &deadpool_postgres::tokio_postgres::Error, pool: &Pool) {
    let status = pool_status(pool);
    tracing::error!(
        error = %error,
        pool.active = status.active,
        pool.idle = status.idle,
        pool.max_size = status.max_size,
        pool.wait_count = status.wait_count,
        "database connection acquisition failed"
    );
}

/// Overload for `deadpool::PoolError` (wraps the underlying pg error).
pub fn log_pool_error(error: &deadpool_postgres::PoolError, pool: &Pool) {
    let status = pool_status(pool);
    tracing::error!(
        error = %error,
        pool.active = status.active,
        pool.idle = status.idle,
        pool.max_size = status.max_size,
        pool.wait_count = status.wait_count,
        "database pool error"
    );
}

/// Generate readiness check entries for the `/ready` endpoint.
///
/// Returns `db_pool: "ok"` when the pool has capacity, or `"degraded"` when
/// all connections are in use and requests are waiting.
pub fn readiness_checks(pool: &Pool) -> std::collections::HashMap<String, String> {
    let mut checks = std::collections::HashMap::new();
    let status = pool_status(pool);
    let pool_health = if status.wait_count > 0 && status.idle == 0 {
        "degraded"
    } else {
        "ok"
    };
    checks.insert("db_pool".to_string(), pool_health.to_string());
    checks.insert("db_pool_active".to_string(), status.active.to_string());
    checks.insert("db_pool_idle".to_string(), status.idle.to_string());
    checks.insert("db_pool_max".to_string(), status.max_size.to_string());
    checks.insert(
        "db_pool_wait_count".to_string(),
        status.wait_count.to_string(),
    );
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_status_fields_are_consistent() {
        let status = PoolStatus {
            active: 3,
            idle: 7,
            max_size: 10,
            wait_count: 0,
        };
        assert_eq!(status.active + status.idle, status.max_size);
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"active\":3"));
        assert!(json.contains("\"idle\":7"));
        assert!(json.contains("\"max_size\":10"));
        assert!(json.contains("\"wait_count\":0"));
    }
}
