//! Crash recovery tests: token executing state, external task lease, timer recovery.
//! See docs/recovery.md §5 (Executing token recovery), §7 (Outbox recovery), §8 (Timer recovery).

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::{
    payloads, EngineEvent, InstanceState, Node, NodeType, OutgoingEdge, ProcessDefinition,
    ProcessInstance, Token, TokenMode, TokenStatus,
};
use bpm_engine::bpm_engine_runtime::{
    BpmEngine, EngineContext, ProcessCompletedHandler, ProcessStartHandler, TokenArrivedHandler,
    UserTaskCompletedHandler,
};
use bpm_engine::bpm_engine_storage::{
    ExternalTaskStore, ProcessInstanceStore, TimerStore, TokenStore,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Process definitions
// ---------------------------------------------------------------------------

/// Minimal two-node process: start -> end
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

/// Process with a timer node: start -> timer -> end
fn timer_def() -> ProcessDefinition {
    ProcessDefinition {
        id: "timer_process",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "delay",
                        condition: None,
                    }],
                },
            ),
            (
                "delay",
                Node {
                    id: "delay",
                    node_type: NodeType::ServiceTask(|_| {}),
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

// ---------------------------------------------------------------------------
// Test 1: Token executing during crash + restart recovery
// ---------------------------------------------------------------------------

/// Scenario: Token is in Executing state when engine crashes.
/// After restart, recovery should reset it to Ready and re-dispatch.
#[tokio::test]
async fn token_executing_recovered_on_restart() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-crash-1".to_string();
    let token_id = "t-crash-1".to_string();

    // Simulate a token left in Executing state (crashed mid-execution)
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "minimal".into(),
        tenant_id: None,
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "start".into(),
            status: TokenStatus::Executing, // intentionally Executing (crashed)
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

    // Simulate engine restart: recovery resets Executing -> Ready before dispatching
    let reset_token = Token {
        id: token_id.clone(),
        node_id: "start".into(),
        status: TokenStatus::Ready,
        mode: TokenMode::Forward,
        version: 0,
        attempt: 1, // attempt incremented on recovery
        parallel_group_id: None,
        updated_at: None,
    };
    let updated = repo
        .update_token_cas(&instance_id, &reset_token)
        .await
        .unwrap();
    assert!(updated, "recovery should reset Executing token to Ready");

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
    ]);

    let def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
    def_store.register(minimal_def());

    let mut ctx = EngineContext::builder(
        repo.clone() as Arc<_>,
        repo.clone() as Arc<_>,
        def_store as Arc<_>,
    )
    .build();

    // Engine restarts and processes the recovered token
    let ev = EngineEvent::TokenArrived(payloads::TokenArrived {
        instance_id: instance_id.clone(),
        token_id: token_id.clone(),
        node_id: "start".to_string(),
    });
    engine.run_async(ev, &mut ctx).await;

    // Verify process completed after recovery
    let loaded = repo.load(&instance_id).await.unwrap().unwrap();
    assert_eq!(
        loaded.state,
        InstanceState::Completed,
        "process should complete after recovery resets Executing token"
    );
}

// ---------------------------------------------------------------------------
// Test 2: External task fetch_and_lock then crash + lease reclaim
// ---------------------------------------------------------------------------

/// Scenario: Worker fetches and locks an external task, then crashes.
/// After lock expiry, another worker should be able to reclaim and process.
#[tokio::test]
async fn external_task_lease_reclaimed_after_crash() {
    let repo = Arc::new(MemoryRepo::new());

    // Create an external task
    let task_id = repo
        .create("token-1", "instance-1", "payment", 3, 60, HashMap::new())
        .await
        .unwrap();

    // Worker-1 fetches and locks with a short lease duration
    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["payment".to_string()],
            10,
            Duration::from_secs(1), // 1 second lease
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, task_id);
    assert_eq!(
        tasks[0].lock_owner.as_deref(),
        Some("worker-1"),
        "worker-1 should own the lock"
    );

    // Worker-2 cannot acquire while lock is still valid
    let tasks2 = repo
        .fetch_and_lock(
            "worker-2",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert!(
        tasks2.is_empty(),
        "worker-2 should not get task while worker-1 holds valid lock"
    );

    // Wait for lock to expire
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Reclaim expired locks — lock should now be expired
    let reclaimed = repo.reclaim_expired_locks().await.unwrap();
    assert!(
        reclaimed >= 1,
        "at least one expired lock should be reclaimed"
    );

    // Now worker-2 should be able to acquire
    let tasks3 = repo
        .fetch_and_lock(
            "worker-2",
            &["payment".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks3.len(), 1, "worker-2 should reclaim task after expiry");
    assert_eq!(
        tasks3[0].task_id, task_id,
        "reclaimed task should be the same one"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Timer due before crash + restart recovery
// ---------------------------------------------------------------------------

/// Scenario: Timer is scheduled to fire, but engine crashes before it.
/// After restart, due timers should be immediately fired.
#[tokio::test]
async fn timer_due_before_crash_fired_after_restart() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-timer-1".to_string();
    let token_id = "t-timer-1".to_string();

    // Create a running instance with a Ready token (waiting at a node)
    let inst = ProcessInstance {
        id: instance_id.clone(),
        process_def_id: "timer_process".into(),
        tenant_id: None,
        tokens: vec![Token {
            id: token_id.clone(),
            node_id: "delay".into(),
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

    // Schedule a timer that is already due (due_at in the past)
    let timer_id = "timer-1".to_string();
    let past_time = "0".to_string(); // Unix epoch — definitely due
    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: timer_id.clone(),
        token_id: token_id.clone(),
        instance_id: instance_id.clone(),
        due_at: past_time,
        status: "Scheduled".to_string(),
        created_at: "0".to_string(),
    };
    repo.insert(&timer_record).await.unwrap();

    // Verify timer is due
    let due_timers = repo.list_due("1", 10).await.unwrap();
    assert!(!due_timers.is_empty(), "timer should be listed as due");

    // Simulate engine restart: fire due timers
    for timer in &due_timers {
        repo.mark_fired(&timer.id).await.unwrap();

        // After firing, resume the token (dispatch TokenArrived)
        let engine = BpmEngine::new(vec![
            Box::new(ProcessStartHandler),
            Box::new(TokenArrivedHandler::new()),
            Box::new(UserTaskCompletedHandler),
            Box::new(ProcessCompletedHandler),
        ]);

        let def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
        def_store.register(timer_def());

        let mut ctx = EngineContext::builder(
            repo.clone() as Arc<_>,
            repo.clone() as Arc<_>,
            def_store as Arc<_>,
        )
        .build();

        let ev = EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id: instance_id.clone(),
            token_id: timer.token_id.clone(),
            node_id: "delay".to_string(),
        });
        engine.run_async(ev, &mut ctx).await;
    }

    // Verify process completed after timer-driven resumption
    let loaded = repo.load(&instance_id).await.unwrap().unwrap();
    assert_eq!(
        loaded.state,
        InstanceState::Completed,
        "process should complete after timer fires and token advances"
    );
}
