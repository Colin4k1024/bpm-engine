//! Invariant tests: parallel join — all branches required, no partial join.
//! See docs/invariants.md §2.

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_storage::ParallelJoinRepo;
use std::sync::Arc;

#[tokio::test]
async fn join_waits_until_all_branches_arrive() {
    let repo = Arc::new(MemoryRepo::new());
    let group_id = "g1".to_string();
    let expected = 2u32;
    repo.ensure_group(&group_id, expected).await.unwrap();
    let done = repo.try_join(&group_id).await.unwrap();
    assert!(
        !done,
        "join must not complete with only 1 arrival when expected 2"
    );
}

#[tokio::test]
async fn join_completes_when_all_branches_arrived() {
    let repo = Arc::new(MemoryRepo::new());
    let group_id = "g1".to_string();
    let expected = 2u32;
    repo.ensure_group(&group_id, expected).await.unwrap();
    let done_one = repo.try_join(&group_id).await.unwrap();
    assert!(
        !done_one,
        "join must not complete with only 1 arrival when expected 2"
    );
    let done_two = repo.try_join(&group_id).await.unwrap();
    assert!(
        done_two,
        "join must complete when all branches have arrived"
    );
}

#[tokio::test]
async fn join_expected_zero_completes_immediately() {
    // When expected=0, the join should complete on the first try_join call
    // because 1 >= 0 is always true.
    let repo = Arc::new(MemoryRepo::new());
    let group_id = "g-zero".to_string();
    repo.ensure_group(&group_id, 0).await.unwrap();
    let done = repo.try_join(&group_id).await.unwrap();
    assert!(
        done,
        "join with expected=0 must complete immediately on first call"
    );
}

#[tokio::test]
async fn join_expected_one_completes_on_first_call() {
    // When expected=1, the first try_join call should complete
    // because the first arrival satisfies the expectation.
    let repo = Arc::new(MemoryRepo::new());
    let group_id = "g-one".to_string();
    repo.ensure_group(&group_id, 1).await.unwrap();
    let done = repo.try_join(&group_id).await.unwrap();
    assert!(done, "join with expected=1 must complete on the first call");
}
