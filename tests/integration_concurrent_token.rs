//! Concurrent token claim: only one of N callers should succeed (CAS).
//! Uses workspace crates: bpm_engine_core, bpm_engine_adapter_memory (async stores).

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::{InstanceState, ProcessInstance, Token, TokenMode, TokenStatus};
use bpm_engine::bpm_engine_storage::{ProcessInstanceStore, TokenStore};
use std::sync::Arc;

#[tokio::test]
async fn only_one_claim_succeeds() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-1".to_string();
    let token_id = "token-1".to_string();
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "p".into(),
        tenant_id: None,
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "n1".into(),
            status: TokenStatus::Ready,
            mode: TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: std::collections::HashMap::new(),
        state: InstanceState::Running,
        version: 0,
        parent_instance_id: None,
        parent_token_id: None,
    };
    repo.save(&inst).await.unwrap();

    let n = 16;
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let r = Arc::clone(&repo);
        let iid = instance_id.clone();
        let tid = token_id.clone();
        handles.push(tokio::spawn(async move {
            r.claim_token(&iid, &tid, 0).await.unwrap_or(false)
        }));
    }
    let mut results = Vec::with_capacity(n);
    for h in handles {
        results.push(h.await.unwrap());
    }
    let success_count = results.iter().filter(|&&b| b).count();
    assert_eq!(success_count, 1, "exactly one claim should succeed");
}
