//! Concurrent token claim: only one of N callers should succeed (CAS).

use bpm_engine::model::{InstanceState, ProcessInstance, Token, TokenStatus};
use bpm_engine::persistence::{MemoryRepo, ProcessInstanceRepo, TokenRepo};
use std::sync::Arc;
use std::thread;

#[test]
fn only_one_claim_succeeds() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-1".to_string();
    let token_id = "token-1".to_string();
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "p".into(),
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "n1".into(),
            status: TokenStatus::Ready,
            mode: bpm_engine::model::TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: std::collections::HashMap::new(),
        state: InstanceState::Running,
        version: 0,
    };
    repo.save(&inst);

    let n = 16;
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let r = Arc::clone(&repo);
        let iid = instance_id.clone();
        let tid = token_id.clone();
        handles.push(thread::spawn(move || r.claim_token(&iid, &tid, 0)));
    }
    let results: Vec<bool> = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    let success_count = results.iter().filter(|&&b| b).count();
    assert_eq!(success_count, 1, "exactly one claim should succeed");
}
