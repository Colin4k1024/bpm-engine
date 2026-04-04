//! Concurrent fetch_and_lock tests — verify that a task is assigned to at most one worker.
//! See ADR-001 for the TOCTOU race condition that was fixed with atomic fetch_and_lock.

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_storage::ExternalTaskStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_fetch_and_lock_guarantees_single_owner() {
    let repo = Arc::new(MemoryRepo::new());

    // Create 10 tasks
    for i in 0..10 {
        repo.create(
            &format!("token-{}", i),
            "inst",
            "payment",
            3,
            60,
            HashMap::new(),
        )
        .await
        .unwrap();
    }

    let n = 16;
    let mut join_set = JoinSet::new();

    for wid in 0..n {
        let repo = Arc::clone(&repo);
        join_set.spawn(async move {
            repo.fetch_and_lock(
                &format!("worker-{}", wid),
                &["payment".to_string()],
                10,
                Duration::from_secs(30),
            )
            .await
            .unwrap()
        });
    }

    let mut results: Vec<Vec<_>> = Vec::new();
    while let Some(result) = join_set.join_next().await {
        results.push(result.unwrap());
    }

    // Flatten all acquired task IDs
    let all_task_ids: Vec<_> = results
        .iter()
        .flatten()
        .map(|t| t.task_id.clone())
        .collect();

    // Verify: each task ID appears at most once across all workers
    let mut seen = std::collections::HashSet::new();
    let mut duplicates = Vec::new();
    for task_id in &all_task_ids {
        if !seen.insert(task_id) {
            duplicates.push(task_id.clone());
        }
    }

    assert!(
        duplicates.is_empty(),
        "tasks must not be assigned to more than one worker, but found duplicates: {:?}",
        duplicates
    );

    // Verify: total tasks assigned equals the number of created tasks (10)
    assert_eq!(
        all_task_ids.len(),
        10,
        "exactly 10 tasks should be assigned across 16 workers"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_fetch_and_lock_all_tasks_eventually_assigned() {
    let repo = Arc::new(MemoryRepo::new());

    // Create a small number of tasks
    let task_count = 5;
    for i in 0..task_count {
        repo.create(
            &format!("token-{}", i),
            "inst",
            "payment",
            3,
            60,
            HashMap::new(),
        )
        .await
        .unwrap();
    }

    let n = 8;
    let mut join_set = JoinSet::new();

    for wid in 0..n {
        let repo = Arc::clone(&repo);
        join_set.spawn(async move {
            repo.fetch_and_lock(
                &format!("worker-{}", wid),
                &["payment".to_string()],
                10,
                Duration::from_secs(30),
            )
            .await
            .unwrap()
        });
    }

    let mut results: Vec<Vec<_>> = Vec::new();
    while let Some(result) = join_set.join_next().await {
        results.push(result.unwrap());
    }

    let total_assigned: usize = results.iter().map(|v| v.len()).sum();

    // All tasks should be assigned to some worker
    assert_eq!(
        total_assigned, task_count,
        "all {} tasks should be assigned across workers",
        task_count
    );
}
