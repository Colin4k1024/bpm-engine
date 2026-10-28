//! Versioned, forward-only SQL migrations (see ADR-005).
//!
//! Migrations are plain `.sql` files in `crates/adapters/postgres/migrations/`,
//! embedded at compile time with [`include_str!`] — there is no runtime file
//! dependency. They are applied in version order; each migration runs in its
//! own transaction together with its `schema_migrations` bookkeeping row, so a
//! migration is either fully applied or not applied at all.
//!
//! # Baseline stamping
//!
//! Databases created by the legacy inline `migrate()` (pre-versioning era)
//! already contain the v1 schema without any `schema_migrations` rows. When
//! the runner finds that shape (v1 tables present, no recorded versions), it
//! stamps everything up to [`BASELINE_VERSION`] as applied instead of
//! re-executing it, then continues with later migrations. An empty database
//! has every migration applied from scratch.
//!
//! # Security
//!
//! All dynamic SQL executed here uses parameter binding (`$1`, `$2`, ...);
//! no query is built with `format!`/concatenation. The migration DDL itself
//! is static text embedded from the `.sql` files at compile time.
//!
//! # Rollback
//!
//! Migrations are forward-only. Rollback strategy (restore from backup /
//! PITR, optionally hand-reviewed down scripts) is documented in
//! `docs/adr/ADR-005-forward-only-sql-migrations.md`.

use deadpool_postgres::Pool;
use std::collections::BTreeSet;
use tokio_postgres::Transaction;
use tracing::info;

/// The last migration version whose schema the legacy inline `migrate()`
/// (pre-versioning era) produced. Databases in that state are stamped up to
/// this version as their baseline instead of re-running those migrations.
const BASELINE_VERSION: u32 = 1;

/// Embedded migrations: `(version, name, sql)` in ascending version order.
const MIGRATIONS: &[(u32, &str, &str)] = &[
    (1, "0001_init", include_str!("../migrations/0001_init.sql")),
    (
        2,
        "0002_query_indexes",
        include_str!("../migrations/0002_query_indexes.sql"),
    ),
    (
        3,
        "0003_idempotency_keys",
        include_str!("../migrations/0003_idempotency_keys.sql"),
    ),
    (
        4,
        "0004_approval_records",
        include_str!("../migrations/0004_approval_records.sql"),
    ),
];

/// Snapshot of migration state, for health checks and startup validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStatus {
    /// Versions recorded in `schema_migrations`, ascending.
    pub applied: Vec<u32>,
    /// Highest applied version, `0` when nothing is applied yet.
    pub latest: u32,
    /// Known migration versions not yet applied, ascending.
    pub pending: Vec<u32>,
}

/// Run all pending migrations, in version order.
///
/// Each migration is applied in its own transaction along with its
/// `schema_migrations` row. A legacy (inline-migrate era) database is
/// baseline-stamped instead of re-executing 0001 (see the module docs).
/// Running this function repeatedly is safe: applied migrations are skipped,
/// so a second run returns an empty `Vec`.
///
/// # Returns
///
/// The versions that were applied by this call (empty when the schema was
/// already up to date).
pub async fn run_migrations(pool: &Pool) -> anyhow::Result<Vec<u32>> {
    let mut client = pool.get().await?;
    ensure_schema_migrations_table(&client).await?;

    let mut applied: BTreeSet<u32> = fetch_applied_versions(&client).await?.into_iter().collect();

    // Baseline stamping: the v1 tables exist but nothing is recorded —
    // an existing deployment from the inline-migrate era. Stamp everything
    // up to BASELINE_VERSION as applied without re-executing it.
    if applied.is_empty() && table_exists(&client, "process_instance").await? {
        for &(version, name, _) in MIGRATIONS.iter().filter(|m| m.0 <= BASELINE_VERSION) {
            stamp_version(&mut client, version, name).await?;
            applied.insert(version);
        }
        info!(
            baseline = BASELINE_VERSION,
            "legacy schema detected; stamped pre-versioning migrations as baseline"
        );
    }

    let mut newly_applied = Vec::new();
    for &(version, name, sql) in MIGRATIONS {
        if applied.contains(&version) {
            continue;
        }
        let tx = client.transaction().await?;
        tx.simple_query(sql).await?;
        stamp_in_tx(&tx, version, name).await?;
        tx.commit().await?;
        applied.insert(version);
        newly_applied.push(version);
        info!(version, name, "applied migration");
    }

    Ok(newly_applied)
}

/// Compute the current migration status without applying anything.
///
/// Reads `schema_migrations` (treating a missing table as "nothing applied")
/// and compares it against the embedded migration list. Works on a fresh,
/// unmigrated database.
pub async fn migration_status(pool: &Pool) -> anyhow::Result<MigrationStatus> {
    let client = pool.get().await?;
    status_from_applied(fetch_applied_versions(&client).await?)
}

/// Validate that the schema is up to date — the deployment contract for
/// servers configured with `migrate_on_start = false`.
///
/// Returns `Ok(())` when no migrations are pending. Otherwise returns an
/// error naming the pending versions and how to enable auto-migration, so
/// startup fails fast instead of running against a stale schema.
pub fn validate_up_to_date(status: &MigrationStatus) -> anyhow::Result<()> {
    if status.pending.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "database schema is out of date: pending migration versions [{}]. \
         Start with database.migrate_on_start = true \
         (BPM_DATABASE__MIGRATE_ON_START=true) to apply them, or run the \
         migrations out of band, then restart.",
        status
            .pending
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Build a [`MigrationStatus`] from the applied-version list.
fn status_from_applied(applied: Vec<u32>) -> anyhow::Result<MigrationStatus> {
    let applied_set: BTreeSet<u32> = applied.iter().copied().collect();
    let pending: Vec<u32> = MIGRATIONS
        .iter()
        .map(|m| m.0)
        .filter(|v| !applied_set.contains(v))
        .collect();
    let latest = *applied.last().unwrap_or(&0);
    Ok(MigrationStatus {
        applied,
        latest,
        pending,
    })
}

/// Create the `schema_migrations` bookkeeping table if it does not exist.
async fn ensure_schema_migrations_table(client: &deadpool_postgres::Object) -> anyhow::Result<()> {
    client
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
            )
            "#,
            &[],
        )
        .await?;
    Ok(())
}

/// Record a version directly (outside a migration transaction), used for
/// baseline stamping of pre-versioning deployments.
async fn stamp_version(
    client: &mut deadpool_postgres::Object,
    version: u32,
    name: &str,
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    stamp_in_tx(&tx, version, name).await?;
    tx.commit().await?;
    Ok(())
}

/// Insert the `schema_migrations` row for an applied migration.
async fn stamp_in_tx(tx: &Transaction<'_>, version: u32, name: &str) -> anyhow::Result<()> {
    tx.execute(
        r#"
        INSERT INTO schema_migrations (version, name, applied_at)
        VALUES ($1, $2, to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'))
        "#,
        &[&(version as i32), &name],
    )
    .await?;
    Ok(())
}

/// Read the applied versions from `schema_migrations` (ascending).
///
/// A missing `schema_migrations` table counts as "nothing applied".
async fn fetch_applied_versions(client: &deadpool_postgres::Object) -> anyhow::Result<Vec<u32>> {
    if !table_exists(client, "schema_migrations").await? {
        return Ok(Vec::new());
    }
    let rows = client
        .query("SELECT version FROM schema_migrations", &[])
        .await?;
    let mut versions: Vec<u32> = rows
        .iter()
        .map(|row| row.get::<_, i32>("version").max(0) as u32)
        .collect();
    versions.sort_unstable();
    versions.dedup();
    Ok(versions)
}

/// Check whether a table exists in the current schema search path.
async fn table_exists(client: &deadpool_postgres::Object, table: &str) -> anyhow::Result<bool> {
    let row = client
        .query_one("SELECT to_regclass($1) IS NOT NULL AS exists", &[&table])
        .await?;
    Ok(row.get("exists"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(applied: &[u32]) -> MigrationStatus {
        status_from_applied(applied.to_vec()).unwrap()
    }

    #[test]
    fn empty_db_reports_everything_pending() {
        let s = status(&[]);
        assert!(s.applied.is_empty());
        assert_eq!(s.latest, 0);
        assert_eq!(s.pending, vec![1, 2, 3, 4]);
    }

    #[test]
    fn fully_applied_db_reports_nothing_pending() {
        let s = status(&[1, 2, 3, 4]);
        assert_eq!(s.applied, vec![1, 2, 3, 4]);
        assert_eq!(s.latest, 4);
        assert!(s.pending.is_empty());
    }

    #[test]
    fn partially_applied_db_reports_rest_pending() {
        let s = status(&[1]);
        assert_eq!(s.applied, vec![1]);
        assert_eq!(s.latest, 1);
        assert_eq!(s.pending, vec![2, 3, 4]);
    }

    #[test]
    fn validate_up_to_date_accepts_current_schema() {
        assert!(validate_up_to_date(&status(&[1, 2, 3, 4])).is_ok());
    }

    #[test]
    fn validate_up_to_date_rejects_pending_migrations() {
        for applied in [vec![], vec![1]] {
            let s = status(&applied);
            let err = validate_up_to_date(&s).expect_err("pending migrations must fail validation");
            let msg = err.to_string();
            assert!(
                msg.contains("pending migration versions"),
                "error should name the pending versions: {msg}"
            );
            for v in &s.pending {
                assert!(
                    msg.contains(&v.to_string()),
                    "error should name version {v}: {msg}"
                );
            }
            assert!(
                msg.contains("migrate_on_start"),
                "error should say how to enable auto-migration: {msg}"
            );
        }
    }

    #[test]
    fn embedded_migrations_are_present_and_ordered() {
        assert!(MIGRATIONS.len() >= 2);
        let mut prev = 0;
        for &(version, name, sql) in MIGRATIONS {
            assert!(version > prev, "migrations must be strictly ascending");
            prev = version;
            assert!(!name.is_empty());
            assert!(
                sql.contains("CREATE"),
                "migration {name} should contain DDL"
            );
        }
        assert!(MIGRATIONS[0].1.contains("0001_init"));
    }
}
