//! Dead letter queue integration tests.

use bpm_engine_adapter_memory::{DeadLetterRepo, MemoryRepo};
use bpm_engine_core::ExternalTaskState;
use bpm_engine_storage::{DeadLetterEntry, DeadLetterStore, ExternalTaskStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn make_entry(id: &str, task_id: &str, task_type: &str) -> DeadLetterEntry {
    DeadLetterEntry {
        id: id.to_string(),
        task_id: task_id.to_string(),
        token_id: "tok-1".to_string(),
        process_instance_id: "inst-1".to_string(),
        task_type: task_type.to_string(),
        error_message: "timeout".to_string(),
        variables: "{}".to_string(),
        tenant_id: None,
        created_at: "1000".to_string(),
    }
}

#[tokio::test]
async fn dlq_insert_list_get_delete_roundtrip() {
    let repo = DeadLetterRepo::new();
    let entry = make_entry("dl-1", "task-1", "payment");
    repo.insert(&entry).await.unwrap();

    let all = repo.list(None, 100).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "dl-1");

    let one = repo.get("dl-1").await.unwrap().unwrap();
    assert_eq!(one.task_type, "payment");
    assert_eq!(one.error_message, "timeout");

    repo.delete("dl-1").await.unwrap();
    assert!(repo.get("dl-1").await.unwrap().is_none());
    assert_eq!(repo.list(None, 100).await.unwrap().len(), 0);
}

#[tokio::test]
async fn dlq_requeue_returns_task_id_and_removes() {
    let repo = DeadLetterRepo::new();
    repo.insert(&make_entry("dl-2", "task-2", "notify"))
        .await
        .unwrap();

    let task_id = repo.requeue("dl-2").await.unwrap();
    assert_eq!(task_id.as_deref(), Some("task-2"));
    assert!(repo.get("dl-2").await.unwrap().is_none());
}

#[tokio::test]
async fn dlq_requeue_nonexistent_returns_none() {
    let repo = DeadLetterRepo::new();
    let task_id = repo.requeue("no-such").await.unwrap();
    assert!(task_id.is_none());
}

#[tokio::test]
async fn dlq_tenant_filter() {
    let repo = DeadLetterRepo::new();
    let mut e1 = make_entry("dl-3", "task-3", "a");
    e1.tenant_id = Some("tenant-a".to_string());
    let mut e2 = make_entry("dl-4", "task-4", "b");
    e2.tenant_id = Some("tenant-b".to_string());
    repo.insert(&e1).await.unwrap();
    repo.insert(&e2).await.unwrap();

    let a_only = repo.list(Some("tenant-a"), 100).await.unwrap();
    assert_eq!(a_only.len(), 1);
    assert_eq!(a_only[0].id, "dl-3");

    let b_only = repo.list(Some("tenant-b"), 100).await.unwrap();
    assert_eq!(b_only.len(), 1);

    let all = repo.list(None, 100).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn dlq_limit_respected() {
    let repo = DeadLetterRepo::new();
    for i in 0..10 {
        repo.insert(&make_entry(&format!("dl-{i}"), &format!("task-{i}"), "x"))
            .await
            .unwrap();
    }

    let limited = repo.list(None, 3).await.unwrap();
    assert_eq!(limited.len(), 3);
}

#[tokio::test]
async fn external_task_fail_goes_to_dlq_when_retries_exhausted() {
    let repo = Arc::new(MemoryRepo::new());
    let dlq = Arc::new(DeadLetterRepo::new());

    // Create a task with 1 retry
    let task_id = repo
        .create("tok-1", "inst-1", "payment", 1, 60, HashMap::new())
        .await
        .unwrap();

    // Fetch and lock
    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].retries, 1);

    // Fail it — retries drop to 0, state = Failed
    repo.fail(&task_id, "worker-1", "boom".to_string(), None)
        .await
        .unwrap();
    let task = repo.get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.state, ExternalTaskState::Failed);

    // Simulate what the REST handler does: insert into DLQ
    let entry = DeadLetterEntry {
        id: "dl-auto".to_string(),
        task_id: task.task_id.clone(),
        token_id: task.token_id.clone(),
        process_instance_id: task.process_instance_id.clone(),
        task_type: task.task_type.clone(),
        error_message: "boom".to_string(),
        variables: serde_json::to_string(&task.variables).unwrap_or_default(),
        tenant_id: None,
        created_at: "1000".to_string(),
    };
    dlq.insert(&entry).await.unwrap();

    let entries = dlq.list(None, 100).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].task_type, "payment");
    assert_eq!(entries[0].error_message, "boom");
}

#[tokio::test]
async fn extend_lock_on_locked_task() {
    let repo = MemoryRepo::new();
    repo.create("tok-1", "inst-1", "job", 3, 60, HashMap::new())
        .await
        .unwrap();

    let tasks = repo
        .fetch_and_lock("worker-1", &["job".to_string()], 10, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);

    // Extend the lock
    let ok = repo
        .extend_lock(&tasks[0].task_id, "worker-1", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(ok);
}

#[tokio::test]
async fn extend_lock_fails_for_wrong_worker() {
    let repo = MemoryRepo::new();
    repo.create("tok-1", "inst-1", "job", 3, 60, HashMap::new())
        .await
        .unwrap();

    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["job".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    let ok = repo
        .extend_lock(&tasks[0].task_id, "worker-2", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(!ok);
}

#[tokio::test]
async fn extend_lock_fails_for_nonexistent_task() {
    let repo = MemoryRepo::new();
    let ok = repo
        .extend_lock("no-such", "worker-1", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(!ok);
}
