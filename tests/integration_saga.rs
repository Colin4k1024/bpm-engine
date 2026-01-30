//! Saga: only Pending records are compensated, in reverse order.

use bpm_engine::engine::{payloads, BpmEngine, EngineContext, EngineEvent, SagaCoordinator};
use bpm_engine::model::{InstanceState, ProcessInstance};
use bpm_engine::persistence::{CompensationRecordRepo, MemoryRepo, ProcessInstanceRepo};
use std::collections::HashMap;
use std::sync::Arc;

fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    format!("{}", d.as_secs())
}

#[test]
fn saga_only_pending_in_reverse_order() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-saga".to_string();
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "p".into(),
        tokens: vec![],
        variables: HashMap::new(),
        state: InstanceState::Running,
        version: 0,
    };
    repo.save(&inst);

    repo.add(&bpm_engine::persistence::CompensationRecordRow {
        id: "rec1".into(),
        instance_id: instance_id.clone(),
        node_id: "node_a".into(),
        handler_ref: "".into(),
        order: 1,
        status: "Completed".into(),
        created_at: utc_now(),
    })
    .unwrap();
    repo.add(&bpm_engine::persistence::CompensationRecordRow {
        id: "rec2".into(),
        instance_id: instance_id.clone(),
        node_id: "node_b".into(),
        handler_ref: "".into(),
        order: 2,
        status: "Pending".into(),
        created_at: utc_now(),
    })
    .unwrap();
    repo.add(&bpm_engine::persistence::CompensationRecordRow {
        id: "rec3".into(),
        instance_id: instance_id.clone(),
        node_id: "node_c".into(),
        handler_ref: "".into(),
        order: 3,
        status: "Pending".into(),
        created_at: utc_now(),
    })
    .unwrap();

    let engine = BpmEngine::new(vec![Box::new(SagaCoordinator)]);
    let mut ctx = EngineContext {
        process_repo: Some(Box::new(Arc::clone(&repo))),
        token_repo: Some(Box::new(Arc::clone(&repo))),
        process_def_repo: None,
        task_repo: None,
        parallel_join_repo: Some(Box::new(Arc::clone(&repo))),
        timer_repo: Some(Box::new(Arc::clone(&repo))),
        compensation_repo: Some(Box::new(Arc::clone(&repo))),
        run_in_tx: None,
    };

    engine.run(
        EngineEvent::TokenFailed(payloads::TokenFailed {
            instance_id: instance_id.clone(),
            token_id: "t1".into(),
            node_id: "n".into(),
            reason: "test".into(),
        }),
        &mut ctx,
    );

    let inst_after = repo.load(&instance_id).unwrap();
    let compensation_tokens: Vec<_> = inst_after
        .tokens
        .iter()
        .filter(|t| t.node_id == "node_b" || t.node_id == "node_c")
        .collect();
    assert_eq!(compensation_tokens.len(), 2, "only Pending (node_b, node_c) get compensation tokens");
    let node_ids: Vec<_> = compensation_tokens.iter().map(|t| t.node_id.as_str()).collect();
    assert!(
        node_ids == ["node_c", "node_b"] || node_ids == ["node_b", "node_c"],
        "order should be reverse: node_c (order 3) then node_b (order 2)"
    );
}
