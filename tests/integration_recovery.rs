//! Recovery-related test: engine continues after state is persisted (simulated restart).
//! Creates instance with Ready token, runs engine with TokenArrived, asserts process completes.

use bpm_engine::bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_engine::bpm_engine_core::{
    payloads, EngineEvent, InstanceState, Node, NodeType, OutgoingEdge, ProcessDefinition,
    ProcessInstance, Token, TokenMode, TokenStatus,
};
use bpm_engine::bpm_engine_runtime::{
    BpmEngine, EngineContext, ProcessCompletedHandler, ProcessStartHandler, TokenArrivedHandler,
    UserTaskCompletedHandler,
};
use bpm_engine::bpm_engine_storage::ProcessInstanceStore;
use std::collections::HashMap;
use std::sync::Arc;

fn minimal_def() -> ProcessDefinition {
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

#[tokio::test]
async fn engine_continues_after_state_persisted() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(minimal_def());

    let instance_id = "inst-recover-1".to_string();
    let token_id = "t1".to_string();
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "minimal".into(),
        tenant_id: None,
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "start".into(),
            status: TokenStatus::Ready,
            mode: TokenMode::Forward,
            version: 0,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: HashMap::new(),
        state: InstanceState::Running,
        version: 0,
    };
    repo.save(&inst).await.unwrap();

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
    ]);

    let mut ctx = EngineContext::builder(
        repo.clone() as Arc<_>,
        repo.clone() as Arc<_>,
        def_store.clone() as Arc<_>,
    )
    .build();

    let ev = EngineEvent::TokenArrived(payloads::TokenArrived {
        instance_id: instance_id.clone(),
        token_id: token_id.clone(),
        node_id: "start".to_string(),
    });
    engine.run_async(ev, &mut ctx).await;

    let loaded = repo.load(&instance_id).await.unwrap();
    assert!(loaded.is_some());
    let inst = loaded.unwrap();
    assert_eq!(
        inst.state,
        InstanceState::Completed,
        "process should be completed after run"
    );
}
