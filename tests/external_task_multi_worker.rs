//! External task multi-worker tests: lease expiry, reclaim, and concurrent lock attempts.
//!
//! Covers:
//! - Two workers competing for the same task (only one succeeds)
//! - Lease expiry causes task to become available for reclaim
//! - Worker completing after lease expiry is rejected
//!
//! See docs/invariants.md §3 (exactly one owner).

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::ExternalTaskState;
use bpm_engine::bpm_engine_storage::ExternalTaskStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn only_one_worker_can_lock_same_task() {
    let repo = Arc::new(MemoryRepo::new());

    let _task_id = repo
        .create("token-1", "instance-1", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();

    // Worker 1 locks the task
    let locked = repo
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(locked.len(), 1);
    assert_eq!(locked[0].state, ExternalTaskState::Locked);
    assert_eq!(locked[0].lock_owner.as_deref(), Some("worker-1"));

    // Worker 2 tries to lock the same task — gets nothing
    let locked2 = repo
        .fetch_and_lock(
            "worker-2",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert!(
        locked2.is_empty(),
        "worker-2 must not see a task already locked by worker-1"
    );
}

#[tokio::test]
async fn lease_expiry_enables_reclaim() {
    let repo = Arc::new(MemoryRepo::new());

    let _task_id = repo
        .create("token-1", "instance-1", "job", 1, 60, HashMap::new())
        .await
        .unwrap();

    // Worker 1 locks with zero-duration lease (expires immediately)
    let tasks = repo
        .fetch_and_lock("worker-1", &["job".to_string()], 10, Duration::from_secs(0))
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);

    // Lease already expired; reclaim should return it to READY
    let reclaimed = repo.reclaim_expired_locks().await.unwrap();
    assert!(reclaimed >= 1, "at least one lock should be reclaimed");

    // Worker 2 can now acquire the task
    let tasks2 = repo
        .fetch_and_lock(
            "worker-2",
            &["job".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks2.len(), 1);
    assert_eq!(tasks2[0].lock_owner.as_deref(), Some("worker-2"));
}

#[tokio::test]
async fn complete_after_lease_expiry_fails() {
    let repo = Arc::new(MemoryRepo::new());

    let task_id = repo
        .create("token-1", "instance-1", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();

    // Worker 1 locks with zero-duration lease
    let _tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            Duration::from_secs(0),
        )
        .await
        .unwrap();

    // Wait for lease to expire
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Reclaim
    let _reclaimed = repo.reclaim_expired_locks().await.unwrap();

    // Worker 1 tries to complete after lease expired — must fail
    let err = repo.complete(&task_id, "worker-1", HashMap::new()).await;
    assert!(
        err.is_err(),
        "complete by worker-1 after lease expiry must be rejected"
    );
}

#[tokio::test]
async fn worker_can_complete_before_lease_expires() {
    let repo = Arc::new(MemoryRepo::new());

    let task_id = repo
        .create("token-1", "instance-1", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();

    let _tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            Duration::from_secs(60),
        )
        .await
        .unwrap();

    // Complete before expiry
    repo.complete(&task_id, "worker-1", HashMap::new())
        .await
        .unwrap();

    let task = repo.get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.state, ExternalTaskState::Completed);
}

#[tokio::test]
async fn fail_decrements_retries_and_returns_to_ready() {
    let repo = Arc::new(MemoryRepo::new());

    let task_id = repo
        .create("token-1", "instance-1", "notify", 2, 60, HashMap::new())
        .await
        .unwrap();

    let _tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["notify".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    // First failure — retries go from 2 to 1, task returns to READY
    repo.fail(&task_id, "worker-1", "timeout".to_string(), None)
        .await
        .unwrap();

    let task = repo.get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.state, ExternalTaskState::Ready);
    assert_eq!(task.retries, 1);

    // Worker 2 can now acquire
    let tasks2 = repo
        .fetch_and_lock(
            "worker-2",
            &["notify".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks2.len(), 1);

    // Second failure — retries go from 1 to 0, task goes to FAILED
    repo.fail(&task_id, "worker-2", "still failing".to_string(), None)
        .await
        .unwrap();

    let task = repo.get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.state, ExternalTaskState::Failed);
    assert_eq!(task.retries, 0);
}
