//! Token idempotency tests: triggering the same token multiple times executes at most once.
//!
//! Invariant: A token reaches a final state exactly once (docs/invariants.md §1).
//!
//! The engine uses optimistic concurrency (CAS) on token version to ensure that
//! triggering an already-executing or completed token is a no-op, not a duplicate execution.

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::{InstanceState, ProcessInstance, Token, TokenStatus};
use bpm_engine::bpm_engine_storage::{ProcessInstanceStore, TokenStore};
use std::sync::Arc;

fn make_token(id: &str, node_id: &str, status: TokenStatus) -> Token {
    Token {
        id: id.into(),
        node_id: node_id.into(),
        status,
        mode: bpm_engine::bpm_engine_core::TokenMode::Forward,
        version: 1,
        attempt: 0,
        parallel_group_id: None,
        updated_at: None,
    }
}

fn make_instance(id: &str, tokens: Vec<Token>) -> ProcessInstance {
    ProcessInstance {
        id: id.into(),
        process_def_id: "process-1".into(),
        state: InstanceState::Running,
        variables: Default::default(),
        tokens,
        tenant_id: None,
        version: 0,
    }
}

#[tokio::test]
async fn claim_token_fails_for_already_claimed_token() {
    let repo = Arc::new(MemoryRepo::new());

    let instance = make_instance(
        "instance-1",
        vec![make_token("token-1", "task-1", TokenStatus::Ready)],
    );
    repo.save(&instance).await.unwrap();

    // First claim succeeds
    let ok1 = repo.claim_token("instance-1", "token-1", 1).await.unwrap();
    assert!(ok1, "first claim should succeed");

    // Second claim with stale version fails
    let ok2 = repo.claim_token("instance-1", "token-1", 1).await.unwrap();
    assert!(!ok2, "claim with stale version should fail");

    // Claim with correct current version also fails (already Executing)
    let ok3 = repo.claim_token("instance-1", "token-1", 2).await.unwrap();
    assert!(!ok3, "claim when already Executing should fail");
}

#[tokio::test]
async fn cannot_claim_completed_token() {
    let repo = Arc::new(MemoryRepo::new());

    let instance = make_instance(
        "instance-1",
        vec![make_token("token-1", "task-1", TokenStatus::Completed)],
    );
    repo.save(&instance).await.unwrap();

    // Attempt to claim a completed token
    let ok = repo.claim_token("instance-1", "token-1", 1).await.unwrap();
    assert!(!ok, "completed token cannot be claimed");
}

#[tokio::test]
async fn cannot_claim_terminated_token() {
    let repo = Arc::new(MemoryRepo::new());

    let instance = make_instance(
        "instance-1",
        vec![make_token("token-1", "task-1", TokenStatus::Terminated)],
    );
    repo.save(&instance).await.unwrap();

    let ok = repo.claim_token("instance-1", "token-1", 1).await.unwrap();
    assert!(!ok, "terminated token cannot be claimed");
}

#[tokio::test]
async fn update_token_cas_rejects_stale_version() {
    let repo = Arc::new(MemoryRepo::new());

    let mut token = make_token("token-1", "task-1", TokenStatus::Executing);
    token.version = 2;

    let instance = make_instance(
        "instance-1",
        vec![make_token("token-1", "task-1", TokenStatus::Executing)],
    );
    repo.save(&instance).await.unwrap();

    // Update with stale version should fail
    let ok = repo.update_token_cas("instance-1", &token).await.unwrap();
    assert!(!ok, "CAS update with stale version should be rejected");

    // Token version in store is still 1, not 2
    let tokens = repo.load_by_instance("instance-1").await.unwrap();
    assert_eq!(tokens[0].version, 1);
}

#[tokio::test]
async fn update_token_cas_succeeds_with_correct_version() {
    let repo = Arc::new(MemoryRepo::new());

    let mut token = make_token("token-1", "task-1", TokenStatus::Completed);
    token.version = 1; // matches stored version

    let instance = make_instance(
        "instance-1",
        vec![make_token("token-1", "task-1", TokenStatus::Executing)],
    );
    repo.save(&instance).await.unwrap();

    // Update with correct version should succeed
    let ok = repo.update_token_cas("instance-1", &token).await.unwrap();
    assert!(ok, "CAS update with correct version should succeed");

    let tokens = repo.load_by_instance("instance-1").await.unwrap();
    assert_eq!(tokens[0].status, TokenStatus::Completed);
}

#[tokio::test]
async fn save_tokens_replaces_all_tokens() {
    let repo = Arc::new(MemoryRepo::new());

    let instance = make_instance(
        "instance-1",
        vec![
            make_token("token-1", "task-1", TokenStatus::Ready),
            make_token("token-2", "task-2", TokenStatus::Ready),
        ],
    );
    repo.save(&instance).await.unwrap();

    // Replace with new token set (e.g., after some completed)
    let new_tokens = vec![
        make_token("token-1", "task-1", TokenStatus::Completed),
        make_token("token-3", "task-3", TokenStatus::Ready),
    ];
    repo.save_tokens("instance-1", &new_tokens).await.unwrap();

    let loaded = repo.load_by_instance("instance-1").await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(loaded
        .iter()
        .any(|t| t.id == "token-1" && t.status == TokenStatus::Completed));
    assert!(loaded
        .iter()
        .any(|t| t.id == "token-3" && t.status == TokenStatus::Ready));
    assert!(
        !loaded.iter().any(|t| t.id == "token-2"),
        "token-2 should be gone"
    );
}
