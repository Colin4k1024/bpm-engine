//! Crash recovery: recover() produces TokenArrived for Ready/Executing tokens; replay completes instance.

use bpm_engine::engine::{
    BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler,
};
use bpm_engine::model::{
    InstanceState, Node, NodeType, OutgoingEdge, ProcessDefinition, ProcessInstance, Token,
    TokenStatus,
};
use bpm_engine::persistence::{MemoryRepo, ProcessDefStore, ProcessInstanceRepo};
use bpm_engine::recovery;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

fn make_minimal_def() -> ProcessDefinition {
    ProcessDefinition {
        id: "minimal",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "end",
                        condition: None,
                    }],
                },
            ),
            (
                "end",
                Node {
                    id: "end",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
        ]),
    }
}

#[test]
fn recover_ready_token_produces_token_arrived() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-recover".to_string();
    let token_id = "t1".to_string();
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "minimal".into(),
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "start".into(),
            status: TokenStatus::Ready,
            mode: bpm_engine::model::TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: HashMap::new(),
        state: InstanceState::Running,
        version: 0,
    };
    repo.save(&inst);

    let mut queue = VecDeque::new();
    recovery::recover(repo.as_ref(), Some(repo.as_ref()), &mut queue);
    assert!(!queue.is_empty());
    let ev = queue.pop_front().unwrap();
    match &ev {
        EngineEvent::TokenArrived(p) => {
            assert_eq!(p.instance_id, instance_id);
            assert_eq!(p.token_id, token_id);
            assert_eq!(p.node_id, "start");
        }
        _ => panic!("expected TokenArrived, got {:?}", ev),
    }
}

#[test]
fn recover_then_run_completes_instance() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = ProcessDefStore::new();
    def_store.register(make_minimal_def());

    let instance_id = "inst-replay".to_string();
    let token_id = "t1".to_string();
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "minimal".into(),
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "start".into(),
            status: TokenStatus::Ready,
            mode: bpm_engine::model::TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: HashMap::new(),
        state: InstanceState::Running,
        version: 0,
    };
    repo.save(&inst);

    let mut queue = VecDeque::new();
    recovery::recover(repo.as_ref(), Some(repo.as_ref()), &mut queue);

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(ProcessCompletedHandler),
    ]);
    let mut ctx = EngineContext {
        process_repo: Some(Box::new(Arc::clone(&repo))),
        token_repo: Some(Box::new(Arc::clone(&repo))),
        process_def_repo: Some(Box::new(def_store)),
        task_repo: None,
        parallel_join_repo: Some(Box::new(Arc::clone(&repo))),
        timer_repo: Some(Box::new(Arc::clone(&repo))),
        compensation_repo: Some(Box::new(Arc::clone(&repo))),
        run_in_tx: Some(Box::new(|event, handlers, ctx, queue| {
            for handler in handlers {
                let new_events = handler.handle(event, ctx);
                queue.extend(new_events);
            }
        })),
    };
    while let Some(ev) = queue.pop_front() {
        engine.run(ev, &mut ctx);
    }
    let inst_after = repo.load(&instance_id).expect("instance exists");
    assert!(inst_after.completed(), "instance should be completed after recovery replay");
}

#[test]
fn recover_without_token_repo_still_emits_token_arrived_for_executing() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-no-tr".to_string();
    let token_id = "t1".to_string();
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "p".into(),
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "n".into(),
            status: TokenStatus::Executing,
            mode: bpm_engine::model::TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: HashMap::new(),
        state: InstanceState::Running,
        version: 0,
    };
    repo.save(&inst);

    let mut queue = VecDeque::new();
    recovery::recover(repo.as_ref(), None, &mut queue);
    assert!(!queue.is_empty(), "without token_repo we still enqueue TokenArrived for Executing");
}
