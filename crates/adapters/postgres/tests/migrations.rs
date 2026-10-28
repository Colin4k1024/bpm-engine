//! Versioned migration tests for the PostgreSQL adapter (issue E1-2).
//!
//! Verifies the deployment contract:
//! - empty database migrates from scratch,
//! - a legacy (inline-migrate era, pre-versioning) schema is baseline-stamped
//!   and upgraded without data loss,
//! - a second run is a no-op,
//! - pending migrations are detectable (startup validation contract).
//!
//! # Dual mode
//!
//! When `BPM_TEST_DATABASE_URL` is set (CI service container), tests connect
//! there instead of spinning up testcontainers. In both modes each test gets
//! an **isolated database** (`bpm_mig_test_<name>`) created from the URL's
//! server, so tests never interfere with each other or with whatever database
//! the URL points at; the isolated database is dropped on teardown
//! (best-effort).
//!
//! Local run (requires Docker):
//!
//! ```text
//! cargo test -p bpm-engine-adapter-postgres --test migrations -- --ignored
//! ```
//!
//! CI mode (no Docker needed):
//!
//! ```text
//! BPM_TEST_DATABASE_URL=postgres://bpm:bpm@localhost:5432/postgres \
//!     cargo test -p bpm-engine-adapter-postgres --test migrations -- --ignored
//! ```

use bpm_engine_adapter_postgres::{
    create_pool, migration_status, run_migrations, validate_up_to_date, PostgresProcessStore,
};
use bpm_engine_storage::ProcessInstanceStore;
use testcontainers::clients;
use testcontainers_modules::postgres::Postgres;

/// The exact SQL of migration 0001, embedded from the same source file the
/// runner uses — used to simulate a legacy (pre-versioning) database.
const MIGRATION_0001_SQL: &str = include_str!("../migrations/0001_init.sql");

const ALL_TABLES: &[&str] = &[
    "process_definition",
    "process_instance",
    "token",
    "external_task",
    "timer",
    "history_event",
    "event_outbox",
    "compensation_record",
    "parallel_join",
    "dead_letter",
    "idempotency_key",
    "approval_record",
    "schema_migrations",
];

/// Isolated test database with its own pool, plus the admin pool (connected
/// to the base URL's database) used to create and drop it.
struct TestDb {
    pool: deadpool_postgres::Pool,
    admin_pool: deadpool_postgres::Pool,
    db_name: String,
    /// Keeps the throwaway container alive for the test duration (local mode).
    _container: Option<testcontainers::Container<'static, Postgres>>,
}

impl TestDb {
    /// Create an isolated database for `test_name`.
    ///
    /// Base URL selection: `BPM_TEST_DATABASE_URL` when set (CI mode),
    /// otherwise a fresh Postgres testcontainer (local mode). The database
    /// named in the URL (defaulting to the `postgres` maintenance DB when the
    /// URL has no path) is used only for `CREATE DATABASE` / `DROP DATABASE`;
    /// all migration assertions run against the isolated database.
    async fn new(test_name: &str) -> Self {
        let (base_url, container) = match std::env::var("BPM_TEST_DATABASE_URL") {
            Ok(url) if !url.trim().is_empty() => {
                println!("using BPM_TEST_DATABASE_URL for isolated test databases");
                (url.trim().to_string(), None)
            }
            _ => {
                let docker: &'static clients::Cli = Box::leak(Box::new(clients::Cli::default()));
                let container = docker.run(Postgres::default());
                let port = container.get_host_port_ipv4(5432);
                (
                    format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
                    Some(container),
                )
            }
        };

        let admin_url = maintenance_url(&base_url).expect("valid base URL");
        let admin_pool = create_pool(&admin_url).expect("admin pool");
        let db_name = isolated_db_name(test_name);

        // Clean up any leftover database from a previous (failed) run.
        drop_database(&admin_pool, &db_name).await;

        {
            let client = admin_pool.get().await.expect("admin client");
            // PostgreSQL identifiers cannot be parameter-bound; `db_name` is
            // generated locally from an allow-listed character set (see
            // `isolated_db_name`), so interpolation here is safe.
            client
                .execute(&format!("CREATE DATABASE {db_name}"), &[])
                .await
                .expect("create isolated test database");
        }

        let test_url = url_with_database(&base_url, &db_name).expect("valid test URL");
        let pool = create_pool(&test_url).expect("test pool");

        TestDb {
            pool,
            admin_pool,
            db_name,
            _container: container,
        }
    }

    /// Drop the isolated database (best-effort). Pooled connections to it are
    /// terminated server-side first so `DROP DATABASE` cannot be blocked.
    async fn teardown(self) {
        self.pool.close();
        drop_database(&self.admin_pool, &self.db_name).await;
        self.admin_pool.close();
    }
}

/// Map a base URL to the database used for maintenance statements. URLs
/// without a database name (empty path) are pointed at the conventional
/// `postgres` maintenance database; URLs with a database name are used as-is.
fn maintenance_url(base: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(base)?;
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/postgres");
    }
    Ok(url.to_string())
}

/// Return the base URL rewritten to point at `db_name`.
fn url_with_database(base: &str, db_name: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(base)?;
    url.set_path(&format!("/{db_name}"));
    Ok(url.to_string())
}

/// Isolated database name for a test: `bpm_mig_test_<sanitized test name>`.
///
/// PostgreSQL identifiers cannot be parameter-bound, so this name is built
/// only from an allow-listed character set (ASCII alphanumeric and `_`).
fn isolated_db_name(test_name: &str) -> String {
    let safe: String = test_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("bpm_mig_test_{safe}")
}

/// Terminate all connections to `db_name` and drop it (best-effort).
async fn drop_database(admin_pool: &deadpool_postgres::Pool, db_name: &str) {
    let client = match admin_pool.get().await {
        Ok(client) => client,
        Err(err) => {
            eprintln!("teardown: could not connect for drop: {err}");
            return;
        }
    };
    // Connection pids are filtered by the datname parameter ($1); the
    // identifier interpolation below is over an allow-listed name.
    let _ = client
        .execute(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid()",
            &[&db_name],
        )
        .await;
    let _ = client
        .execute(&format!("DROP DATABASE IF EXISTS {db_name}"), &[])
        .await
        .inspect_err(|err| eprintln!("teardown: drop database failed: {err}"));
}

/// Check that a relation (table or index) exists in the current search path.
async fn object_exists(client: &deadpool_postgres::Object, name: &str) -> bool {
    client
        .query_one("SELECT to_regclass($1) IS NOT NULL AS exists", &[&name])
        .await
        .expect("to_regclass query should succeed")
        .get::<_, bool>("exists")
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn empty_db_migrates_from_scratch() {
    let db = TestDb::new("empty_db_migrates_from_scratch").await;
    let pool = db.pool.clone();

    let applied = run_migrations(&pool).await.expect("run_migrations failed");
    assert_eq!(
        applied,
        vec![1, 2, 3, 4],
        "fresh database applies all migrations"
    );

    let status = migration_status(&pool)
        .await
        .expect("migration_status failed");
    assert_eq!(status.applied, vec![1, 2, 3, 4]);
    assert_eq!(status.latest, 4);
    assert!(status.pending.is_empty());

    {
        let client = pool.get().await.expect("client");
        for table in ALL_TABLES {
            assert!(
                object_exists(&client, table).await,
                "table {table} should exist after migrating"
            );
        }
        // 0002 indexes were applied
        assert!(object_exists(&client, "idx_process_instance_state").await);
        assert!(object_exists(&client, "idx_token_instance").await);
        // 0003 table was applied
        assert!(object_exists(&client, "idempotency_key").await);

        // The schema is usable: a round-trip works.
        client
            .execute(
                "INSERT INTO process_instance (id, process_def_id, state, version) \
                 VALUES ($1, $2, 'Running', 1)",
                &[&"sanity-1", &"def-1"],
            )
            .await
            .expect("insert into migrated schema");
        let count: i64 = client
            .query_one("SELECT COUNT(*) FROM process_instance", &[])
            .await
            .expect("count")
            .get(0);
        assert_eq!(count, 1);
    }

    db.teardown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn legacy_schema_is_baselined_and_upgraded() {
    let db = TestDb::new("legacy_schema_is_baselined_and_upgraded").await;
    let pool = db.pool.clone();

    // Legacy database: created by the inline migrate() era — only the 0001
    // schema exists, with no schema_migrations bookkeeping.
    {
        let client = pool.get().await.expect("client");
        client
            .simple_query(MIGRATION_0001_SQL)
            .await
            .expect("apply 0001 to simulate legacy schema");
        // Data from that era must survive the upgrade.
        client
            .execute(
                "INSERT INTO process_instance (id, process_def_id, state, version) \
                 VALUES ($1, $2, 'Running', 1)",
                &[&"legacy-inst", &"legacy-def"],
            )
            .await
            .expect("seed legacy instance");
        client
            .execute(
                "INSERT INTO token (id, instance_id, node_id, status, mode, version, attempt) \
                 VALUES ($1, $2, $3, 'Waiting', 'Forward', 1, 0)",
                &[&"legacy-token", &"legacy-inst", &"start"],
            )
            .await
            .expect("seed legacy token");
    }

    let applied = run_migrations(&pool).await.expect("run_migrations failed");
    // 0001 was baseline-STAMPED (recorded, not executed by this call);
    // only 0002, 0003 and 0004 were actually applied here.
    assert_eq!(
        applied,
        vec![2, 3, 4],
        "legacy schema: 0001 stamped as baseline, 0002-0004 applied"
    );

    let status = migration_status(&pool)
        .await
        .expect("migration_status failed");
    assert_eq!(status.applied, vec![1, 2, 3, 4]);
    assert_eq!(status.latest, 4);
    assert!(status.pending.is_empty());

    {
        let client = pool.get().await.expect("client");
        // Baseline rows survived the upgrade.
        let state: String = client
            .query_one(
                "SELECT state FROM process_instance WHERE id = $1",
                &[&"legacy-inst"],
            )
            .await
            .expect("legacy instance row should survive")
            .get(0);
        assert_eq!(state, "Running");
        let instances: i64 = client
            .query_one("SELECT COUNT(*) FROM process_instance", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(instances, 1);
        let tokens: i64 = client
            .query_one(
                "SELECT COUNT(*) FROM token WHERE instance_id = $1",
                &[&"legacy-inst"],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(tokens, 1);

        // 0002 really ran on top of the baseline: its indexes exist.
        assert!(object_exists(&client, "idx_process_instance_state").await);
        assert!(object_exists(&client, "idx_token_instance").await);
    }

    db.teardown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn second_run_is_noop() {
    let db = TestDb::new("second_run_is_noop").await;
    let pool = db.pool.clone();

    let first = run_migrations(&pool).await.expect("first run failed");
    assert_eq!(first, vec![1, 2, 3, 4]);

    let second = run_migrations(&pool).await.expect("second run failed");
    assert!(
        second.is_empty(),
        "second run must apply nothing, got {second:?}"
    );

    let status = migration_status(&pool)
        .await
        .expect("migration_status failed");
    assert_eq!(status.applied, vec![1, 2, 3, 4]);
    assert_eq!(status.latest, 4);
    assert!(status.pending.is_empty());

    db.teardown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn pending_detected_when_not_applied() {
    let db = TestDb::new("pending_detected_when_not_applied").await;
    let pool = db.pool.clone();

    // Fresh, unmigrated database: everything pending.
    let status = migration_status(&pool)
        .await
        .expect("migration_status failed");
    assert_eq!(status.applied, Vec::<u32>::new());
    assert_eq!(status.latest, 0);
    assert_eq!(status.pending, vec![1, 2, 3, 4]);

    // The server-level startup contract (migrate_on_start = false): a
    // schema with pending migrations must fail validation, naming the
    // pending versions.
    let err =
        validate_up_to_date(&status).expect_err("pending migrations must fail startup validation");
    let msg = err.to_string();
    assert!(msg.contains("pending migration versions"), "got: {msg}");
    assert!(
        msg.contains("1, 2, 3, 4"),
        "error should name versions 1, 2, 3, 4: {msg}"
    );
    assert!(
        msg.contains("migrate_on_start"),
        "error should explain how to enable auto-migration: {msg}"
    );

    // After migrating, the same check passes.
    run_migrations(&pool).await.expect("run_migrations failed");
    let status = migration_status(&pool)
        .await
        .expect("migration_status failed");
    assert!(status.pending.is_empty());
    assert!(
        validate_up_to_date(&status).is_ok(),
        "schema up to date must pass startup validation"
    );

    db.teardown().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn idempotency_key_store_round_trip_after_migration() {
    // E2-4: migration 0003 creates the idempotency_key table; the process
    // store round-trips keys over it (find / save / tenant scoping / upsert
    // / list_page windowing).
    let db = TestDb::new("idempotency_key_store_round_trip").await;
    let pool = db.pool.clone();

    let applied = run_migrations(&pool).await.expect("run_migrations failed");
    assert_eq!(applied, vec![1, 2, 3, 4]);

    let store = PostgresProcessStore::new(pool.clone());

    // Unknown key is None, not an error.
    assert_eq!(
        store.find_idempotency_key("k1", None).await.unwrap(),
        None,
        "unknown key must return Ok(None)"
    );

    // Record and find (no tenant).
    store
        .save_idempotency_key("k1", None, "inst-1")
        .await
        .unwrap();
    assert_eq!(
        store.find_idempotency_key("k1", None).await.unwrap(),
        Some("inst-1".to_string())
    );

    // Tenant scoping: same key, different tenant → different mapping.
    assert_eq!(
        store
            .find_idempotency_key("k1", Some("tenant-a"))
            .await
            .unwrap(),
        None,
        "key recorded without tenant must not leak into a tenant scope"
    );
    store
        .save_idempotency_key("k1", Some("tenant-a"), "inst-2")
        .await
        .unwrap();
    assert_eq!(
        store
            .find_idempotency_key("k1", Some("tenant-a"))
            .await
            .unwrap(),
        Some("inst-2".to_string())
    );
    assert_eq!(
        store.find_idempotency_key("k1", None).await.unwrap(),
        Some("inst-1".to_string())
    );

    // Upsert overwrites the mapping.
    store
        .save_idempotency_key("k1", None, "inst-3")
        .await
        .unwrap();
    assert_eq!(
        store.find_idempotency_key("k1", None).await.unwrap(),
        Some("inst-3".to_string())
    );

    // The (key, tenant) uniqueness is enforced by the PRIMARY KEY: only one
    // row exists per scope after upserts.
    {
        let client = pool.get().await.expect("client");
        let rows: i64 = client
            .query_one("SELECT COUNT(*) FROM idempotency_key", &[])
            .await
            .expect("count idempotency_key")
            .get(0);
        assert_eq!(rows, 2, "two scopes: no-tenant and tenant-a");
    }

    // list_page: window over the deterministic id ordering.
    for id in ["page-a", "page-b", "page-c"] {
        let client = pool.get().await.expect("client");
        client
            .execute(
                "INSERT INTO process_instance (id, process_def_id, state, version) \
                 VALUES ($1, 'def-1', 'Running', 1)",
                &[&id],
            )
            .await
            .expect("seed instance");
    }
    assert_eq!(
        store.list_page(None, 1, 1).await.unwrap(),
        vec!["page-b".to_string()]
    );
    assert!(store.list_page(None, 100, 10).await.unwrap().is_empty());

    db.teardown().await;
}
