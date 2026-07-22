//! Integration tests for PostgreSQL adapter stores using testcontainers.
//!
//! These tests spin up a real PostgreSQL container and exercise the store implementations.
//! Requires Docker. Run with: `cargo test -p bpm-engine-adapter-postgres --test integration`

use bpm_engine_adapter_postgres::{create_pool, migrate, PostgresTokenStore};
use bpm_engine_core::{Token, TokenMode, TokenStatus};
use bpm_engine_storage::TokenStore;
use testcontainers::clients;
use testcontainers_modules::postgres::Postgres;

async fn setup_pool() -> (
    deadpool_postgres::Pool,
    testcontainers::Container<'static, Postgres>,
) {
    let docker: &'static clients::Cli = Box::leak(Box::new(clients::Cli::default()));
    let container = docker.run(Postgres::default());
    let port = container.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);
    let pool = create_pool(&url).expect("failed to create pool");
    migrate(&pool).await.expect("migration failed");
    (pool, container)
}

async fn insert_test_instance(pool: &deadpool_postgres::Pool, id: &str) {
    let client = pool.get().await.unwrap();
    client
        .execute(
            "INSERT INTO process_instance (id, process_def_id, state, version) VALUES ($1, 'def-1', 'Running', 1)",
            &[&id],
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn token_store_save_and_load() {
    let (pool, _container) = setup_pool().await;
    let store = PostgresTokenStore::new(pool.clone());
    let instance_id = "inst-1";

    insert_test_instance(&pool, instance_id).await;

    let tokens = vec![
        Token {
            id: "t1".into(),
            node_id: "start".into(),
            status: TokenStatus::Ready,
            mode: TokenMode::Forward,
            version: 1,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        },
        Token {
            id: "t2".into(),
            node_id: "task-1".into(),
            status: TokenStatus::Waiting,
            mode: TokenMode::Forward,
            version: 1,
            attempt: 0,
            parallel_group_id: Some("pg-1".into()),
            updated_at: Some("2024-01-01T00:00:00Z".into()),
        },
    ];

    store.save_tokens(instance_id, &tokens).await.unwrap();
    let loaded = store.load_by_instance(instance_id).await.unwrap();
    assert_eq!(loaded.len(), 2);

    let t1 = loaded.iter().find(|t| t.id == "t1").unwrap();
    assert_eq!(t1.status, TokenStatus::Ready);
    assert_eq!(t1.node_id, "start");

    let t2 = loaded.iter().find(|t| t.id == "t2").unwrap();
    assert_eq!(t2.status, TokenStatus::Waiting);
    assert_eq!(t2.parallel_group_id, Some("pg-1".into()));
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn token_store_claim_token() {
    let (pool, _container) = setup_pool().await;
    let store = PostgresTokenStore::new(pool.clone());
    let instance_id = "inst-claim";

    insert_test_instance(&pool, instance_id).await;

    let tokens = vec![Token {
        id: "t-ready".into(),
        node_id: "task-1".into(),
        status: TokenStatus::Ready,
        mode: TokenMode::Forward,
        version: 1,
        attempt: 0,
        parallel_group_id: None,
        updated_at: None,
    }];
    store.save_tokens(instance_id, &tokens).await.unwrap();

    let claimed = store.claim_token(instance_id, "t-ready", 1).await.unwrap();
    assert!(claimed, "should claim Ready token");

    let claimed_again = store.claim_token(instance_id, "t-ready", 1).await.unwrap();
    assert!(
        !claimed_again,
        "double-claim should fail (version mismatch)"
    );

    let loaded = store.load_by_instance(instance_id).await.unwrap();
    let token = loaded.iter().find(|t| t.id == "t-ready").unwrap();
    assert_eq!(token.status, TokenStatus::Executing);
    assert_eq!(token.version, 2);
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn token_store_update_cas() {
    let (pool, _container) = setup_pool().await;
    let store = PostgresTokenStore::new(pool.clone());
    let instance_id = "inst-cas";

    insert_test_instance(&pool, instance_id).await;

    let tokens = vec![Token {
        id: "t-cas".into(),
        node_id: "task-1".into(),
        status: TokenStatus::Executing,
        mode: TokenMode::Forward,
        version: 1,
        attempt: 0,
        parallel_group_id: None,
        updated_at: None,
    }];
    store.save_tokens(instance_id, &tokens).await.unwrap();

    let updated_token = Token {
        id: "t-cas".into(),
        node_id: "task-1".into(),
        status: TokenStatus::Completed,
        mode: TokenMode::Forward,
        version: 2,
        attempt: 0,
        parallel_group_id: None,
        updated_at: Some("2024-01-02T00:00:00Z".into()),
    };

    let success = store
        .update_token_cas(instance_id, &updated_token)
        .await
        .unwrap();
    assert!(success, "CAS should succeed with matching version");

    let stale_token = Token {
        id: "t-cas".into(),
        node_id: "task-1".into(),
        status: TokenStatus::Terminated,
        mode: TokenMode::Forward,
        version: 2,
        attempt: 0,
        parallel_group_id: None,
        updated_at: None,
    };

    let fail = store
        .update_token_cas(instance_id, &stale_token)
        .await
        .unwrap();
    assert!(!fail, "CAS should fail with stale version");
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn process_def_store_deploy_and_load() {
    use bpm_engine_adapter_postgres::PostgresProcessDefStore;
    use bpm_engine_storage::ProcessDefinitionStore;

    let (pool, _container) = setup_pool().await;
    let store = PostgresProcessDefStore::new(pool);

    let bpmn_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="simple-process" isExecutable="true">
    <startEvent id="start"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#;

    store.deploy("simple-1", bpmn_xml).await.unwrap();

    let def = store.load("simple-1").await.unwrap();
    assert!(def.is_some(), "should load deployed definition");
    let def = def.unwrap();
    assert_eq!(def.id, "simple-process");
    assert!(def.nodes.contains_key("start"));
    assert!(def.nodes.contains_key("end"));

    let missing = store.load("nonexistent").await.unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn process_store_save_and_load() {
    use bpm_engine_adapter_postgres::PostgresProcessStore;
    use bpm_engine_core::{InstanceState, ProcessInstance};
    use bpm_engine_storage::ProcessInstanceStore;

    let (pool, _container) = setup_pool().await;
    let store = PostgresProcessStore::new(pool);

    let instance = ProcessInstance {
        id: "inst-ps-1".into(),
        process_def_id: "def-1".into(),
        tenant_id: Some("tenant-a".into()),
        tokens: vec![],
        variables: std::collections::HashMap::new(),
        state: InstanceState::Running,
        version: 1,
        parent_instance_id: None,
        parent_token_id: None,
    };

    store.save(&instance).await.unwrap();

    let loaded = store.load("inst-ps-1").await.unwrap();
    assert!(loaded.is_some());
    let loaded = loaded.unwrap();
    assert_eq!(loaded.process_def_id, "def-1");
    assert_eq!(loaded.state, InstanceState::Running);

    let running = store.list_running(Some("tenant-a")).await.unwrap();
    assert!(running.contains(&"inst-ps-1".to_string()));

    let other_tenant = store.list_running(Some("tenant-b")).await.unwrap();
    assert!(other_tenant.is_empty());
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn timer_store_insert_and_list_due() {
    use bpm_engine_adapter_postgres::PostgresTimerStore;
    use bpm_engine_storage::{TimerRecord, TimerStore};

    let (pool, _container) = setup_pool().await;

    let client = pool.get().await.unwrap();
    client
        .execute(
            "INSERT INTO process_instance (id, process_def_id, state, version) VALUES ('inst-timer', 'def-1', 'Running', 1)",
            &[],
        )
        .await
        .unwrap();

    let store = PostgresTimerStore::new(pool);

    let record = TimerRecord {
        id: "timer-1".into(),
        token_id: "tok-1".into(),
        instance_id: "inst-timer".into(),
        node_id: "node-1".into(),
        due_at: "1000".into(),
        status: "Scheduled".into(),
        created_at: "500".into(),
    };
    store.insert(&record).await.unwrap();

    let due = store.list_due("1000", 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, "timer-1");

    let not_due = store.list_due("999", 10).await.unwrap();
    assert!(not_due.is_empty());

    store.mark_fired("timer-1").await.unwrap();
    let after_fire = store.list_due("2000", 10).await.unwrap();
    assert!(
        after_fire.is_empty(),
        "fired timers should not appear in due list"
    );
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn external_task_store_create_fetch_complete() {
    use bpm_engine_adapter_postgres::PostgresExternalTaskStore;
    use bpm_engine_storage::ExternalTaskStore;
    use std::collections::HashMap;

    let (pool, _container) = setup_pool().await;
    insert_test_instance(&pool, "inst-et-1").await;
    let store = PostgresExternalTaskStore::new(pool);

    // Create an external task
    let task_id = store
        .create("tok-et-1", "inst-et-1", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();
    assert!(!task_id.is_empty());

    // Fetch and lock
    let tasks = store
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_type, "payment");

    // Complete
    store
        .complete(&tasks[0].task_id, "worker-1", HashMap::new())
        .await
        .unwrap();

    // After completion, fetch returns empty
    let remaining = store
        .fetch_and_lock(
            "worker-2",
            &["payment".to_string()],
            10,
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert!(
        remaining.is_empty(),
        "completed task should not be fetched again"
    );
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn external_task_store_fail_and_reclaim() {
    use bpm_engine_adapter_postgres::PostgresExternalTaskStore;
    use bpm_engine_storage::ExternalTaskStore;
    use std::collections::HashMap;

    let (pool, _container) = setup_pool().await;
    insert_test_instance(&pool, "inst-et-2").await;
    let store = PostgresExternalTaskStore::new(pool);

    store
        .create("tok-et-2", "inst-et-2", "email", 3, 60, HashMap::new())
        .await
        .unwrap();

    let tasks = store
        .fetch_and_lock(
            "worker-1",
            &["email".to_string()],
            10,
            std::time::Duration::from_millis(1), // very short lock
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);

    // Fail the task (retries decrement)
    store
        .fail(
            &tasks[0].task_id,
            "worker-1",
            "timeout".to_string(),
            Some(std::time::Duration::from_millis(1)),
        )
        .await
        .unwrap();

    // Wait for lock to expire
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Reclaim expired locks
    let reclaimed = store.reclaim_expired_locks().await.unwrap();
    assert!(reclaimed >= 1, "expired lock should be reclaimed");
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn history_repo_append_and_list() {
    use bpm_engine_adapter_postgres::PostgresHistoryRepo;
    use bpm_engine_storage::HistoryRepo;

    let (pool, _container) = setup_pool().await;
    insert_test_instance(&pool, "inst-hist-1").await;
    let repo = PostgresHistoryRepo::new(pool);

    // Append events
    let id1 = repo
        .append(
            "inst-hist-1",
            "ProcessStarted",
            &serde_json::json!({"instance_id": "inst-hist-1"}),
            "1000",
        )
        .await
        .unwrap();
    assert!(!id1.is_empty());

    let _id2 = repo
        .append(
            "inst-hist-1",
            "TokenArrived",
            &serde_json::json!({"node_id": "task-1"}),
            "1001",
        )
        .await
        .unwrap();

    let _id3 = repo
        .append(
            "inst-hist-1",
            "ProcessCompleted",
            &serde_json::json!({"node_id": "end"}),
            "1002",
        )
        .await
        .unwrap();

    // List all events
    let loaded = repo
        .list_by_instance("inst-hist-1", None, None)
        .await
        .unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].event_type, "ProcessStarted");
    assert_eq!(loaded[1].event_type, "TokenArrived");
    assert_eq!(loaded[2].event_type, "ProcessCompleted");

    // Filter by event_type
    let filtered = repo
        .list_by_instance("inst-hist-1", None, Some("TokenArrived"))
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].event_type, "TokenArrived");

    // Nonexistent instance returns empty
    let empty = repo
        .list_by_instance("nonexistent", None, None)
        .await
        .unwrap();
    assert!(empty.is_empty());
}
