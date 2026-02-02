//! Invariant tests: external task — exactly one owner, lease expiry, retries monotonic.
//! See docs/invariants.md §3.

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::ExternalTaskState;
use bpm_engine::bpm_engine_storage::ExternalTaskStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn external_task_exactly_one_owner_when_locked() {
    let repo = Arc::new(MemoryRepo::new());
    let _ = repo
        .create("token-1", "instance-1", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();
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
    assert_eq!(tasks[0].state, ExternalTaskState::Locked);
    assert_eq!(tasks[0].lock_owner.as_deref(), Some("worker-1"));

    let tasks2 = repo
        .fetch_and_lock(
            "worker-2",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert!(
        tasks2.is_empty(),
        "task already locked by worker-1 must not be given to worker-2"
    );
}

#[tokio::test]
async fn external_task_complete_only_by_owner() {
    let repo = Arc::new(MemoryRepo::new());
    let task_id = repo
        .create("token-1", "instance-1", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();
    let _ = repo
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let err = repo.complete(&task_id, "worker-2", HashMap::new()).await;
    assert!(err.is_err(), "complete by non-owner must be rejected");
}

#[tokio::test]
async fn external_task_retries_monotonic() {
    let repo = Arc::new(MemoryRepo::new());
    let task_id = repo
        .create("token-1", "instance-1", "notify", 2, 60, HashMap::new())
        .await
        .unwrap();
    let _ = repo
        .fetch_and_lock(
            "worker-1",
            &["notify".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    repo.fail(&task_id, "worker-1", "error".to_string(), None)
        .await
        .unwrap();
    let task = repo.get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.retries, 1, "retries must decrease on fail");
    assert_eq!(task.state, ExternalTaskState::Ready);
    let _ = repo
        .fetch_and_lock(
            "worker-1",
            &["notify".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    repo.fail(&task_id, "worker-1", "error2".to_string(), None)
        .await
        .unwrap();
    let task = repo.get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.state, ExternalTaskState::Failed);
    assert_eq!(task.retries, 0);
}
