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
    assert!(!done, "join must not complete with only 1 arrival when expected 2");
}

#[tokio::test]
async fn join_completes_when_all_branches_arrived() {
    let repo = Arc::new(MemoryRepo::new());
    let group_id = "g1".to_string();
    let expected = 2u32;
    repo.ensure_group(&group_id, expected).await.unwrap();
    let done_one = repo.try_join(&group_id).await.unwrap();
    assert!(!done_one, "join must not complete with only 1 arrival when expected 2");
    let done_two = repo.try_join(&group_id).await.unwrap();
    assert!(done_two, "join must complete when all branches have arrived");
}
