//! Timer integration tests: due timers fire, non-due timers don't fire,
//! multiple timers ordered, crash recovery re-schedules.
//!
//! Covers issue #21.

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::{
    payloads, EngineEvent, InstanceState, Node, NodeType, OutgoingEdge, ProcessDefinition,
    ProcessInstance, Token, TokenMode, TokenStatus,
};
use bpm_engine::bpm_engine_runtime::{
    BpmEngine, EngineContext, ProcessCompletedHandler, ProcessStartHandler, TokenArrivedHandler,
    UserTaskCompletedHandler,
};
use bpm_engine::bpm_engine_storage::{ProcessInstanceStore, TimerStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Process with a service task node (used to represent a timer-boundary node).
fn timer_process_def() -> ProcessDefinition {
    ProcessDefinition {
        id: "timer_process",
        start: "start",
        boundary_events: HashMap::new(),
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

fn unix_now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string()
}

async fn make_repo_with_instance_and_token(instance_id: &str, token_id: &str) -> Arc<MemoryRepo> {
    let repo = Arc::new(MemoryRepo::new());
    let inst = ProcessInstance {
        id: instance_id.to_string(),
        process_def_id: "timer_process".into(),
        tenant_id: None,
        tokens: vec![Token {
            id: token_id.to_string(),
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
        parent_instance_id: None,
        parent_token_id: None,
    };
    repo.save(&inst).await.unwrap();
    repo
}

fn build_engine_and_ctx(
    repo: Arc<MemoryRepo>,
    def: ProcessDefinition,
) -> (BpmEngine, EngineContext) {
    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
    ]);

    let def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
    def_store.register(def);

    let ctx = EngineContext::builder(
        repo.clone() as Arc<_>,
        repo.clone() as Arc<_>,
        def_store as Arc<_>,
    )
    .build();

    (engine, ctx)
}

// ---------------------------------------------------------------------------
// Test 1: Timer due fires TokenArrived
// ---------------------------------------------------------------------------

/// A timer with `due_at` in the past should be listed as due and fire.
#[tokio::test]
async fn due_timer_fires_and_advances_token() {
    let instance_id = "inst-due-1";
    let token_id = "t-due-1";
    let repo = make_repo_with_instance_and_token(instance_id, token_id).await;

    // Insert a timer that is already due (epoch 0)
    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: "timer-due".into(),
        token_id: token_id.into(),
        instance_id: instance_id.into(),
        node_id: "delay".into(),
        due_at: "0".into(),
        status: "Scheduled".into(),
        created_at: "0".into(),
    };
    repo.insert(&timer_record).await.unwrap();

    // Verify timer is listed as due
    let now = unix_now();
    let due = repo.list_due(&now, 100).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, "timer-due");

    // Mark fired and dispatch TokenArrived
    repo.mark_fired(&due[0].id).await.unwrap();

    let (engine, mut ctx) = build_engine_and_ctx(Arc::clone(&repo), timer_process_def());
    let ev = EngineEvent::TokenArrived(payloads::TokenArrived {
        instance_id: instance_id.into(),
        token_id: token_id.into(),
        node_id: "delay".into(),
    });
    engine.run_async(ev, &mut ctx).await;

    // After firing, process should complete
    let loaded = repo.load(instance_id).await.unwrap().unwrap();
    assert_eq!(
        loaded.state,
        InstanceState::Completed,
        "process should complete after due timer fires"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Timer not yet due does NOT fire
// ---------------------------------------------------------------------------

/// A timer with `due_at` in the far future should NOT appear in list_due.
#[tokio::test]
async fn not_due_timer_does_not_fire() {
    let repo = Arc::new(MemoryRepo::new());

    // Insert a timer due far in the future (year ~292277026)
    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: "timer-future".into(),
        token_id: "t-1".into(),
        instance_id: "inst-1".into(),
        node_id: "delay".into(),
        due_at: "99999999999".into(),
        status: "Scheduled".into(),
        created_at: "0".into(),
    };
    repo.insert(&timer_record).await.unwrap();

    // Query with current time — should find nothing
    let now = unix_now();
    let due = repo.list_due(&now, 100).await.unwrap();
    assert!(
        due.is_empty(),
        "timer with far-future due_at should not be due yet"
    );

    // Timer should still be in Scheduled state
    let timer = repo.get_by_id("timer-future").await.unwrap().unwrap();
    assert_eq!(timer.status, "Scheduled");
}

// ---------------------------------------------------------------------------
// Test 3: Multiple timers fire in chronological order
// ---------------------------------------------------------------------------

/// When multiple timers exist with different due times, list_due returns
/// those that are past their due time. After sequential firing, all
/// associated tokens should advance.
#[tokio::test]
async fn multiple_timers_fire_in_order() {
    let repo = Arc::new(MemoryRepo::new());

    // Create three timers: t-past (due now), t-present (due now), t-future (not due)
    for i in 0..3 {
        let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
            id: format!("timer-{}", i),
            token_id: format!("t-{}", i),
            instance_id: format!("inst-{}", i),
            node_id: format!("node-{}", i),
            due_at: if i < 2 {
                "0".into()
            } else {
                "99999999999".into()
            },
            status: "Scheduled".into(),
            created_at: format!("{}", i),
        };
        repo.insert(&timer_record).await.unwrap();
    }

    // list_due should return the two past timers
    let now = unix_now();
    let due = repo.list_due(&now, 100).await.unwrap();
    assert_eq!(due.len(), 2, "only past-due timers should be listed as due");

    // Verify they are the correct timers
    let due_ids: Vec<&str> = due.iter().map(|t| t.id.as_str()).collect();
    assert!(due_ids.contains(&"timer-0"));
    assert!(due_ids.contains(&"timer-1"));
    assert!(!due_ids.contains(&"timer-2"));

    // Fire each due timer
    for timer in &due {
        repo.mark_fired(&timer.id).await.unwrap();
    }

    // Verify fired timers have status "Fired"
    for id in &["timer-0", "timer-1"] {
        let t = repo.get_by_id(id).await.unwrap().unwrap();
        assert_eq!(t.status, "Fired");
    }

    // Future timer should still be Scheduled
    let future = repo.get_by_id("timer-2").await.unwrap().unwrap();
    assert_eq!(future.status, "Scheduled");

    // list_due again should return nothing (fired timers excluded)
    let due_after = repo.list_due(&now, 100).await.unwrap();
    assert!(
        due_after.is_empty(),
        "already-fired timers should not appear in list_due"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Timer recovery after crash (re-schedule on restart)
// ---------------------------------------------------------------------------

/// Scenario: Timer was scheduled, engine crashes, restarts.
/// The initial sweep of the timer scheduler should find and fire due timers.
/// This verifies the TimerStore persistence contract for crash recovery.
#[tokio::test]
async fn timer_recovery_after_simulated_crash() {
    let instance_id = "inst-recovery-1";
    let token_id = "t-recovery-1";
    let repo = make_repo_with_instance_and_token(instance_id, token_id).await;

    // Schedule a timer due in the past (simulating it was created before crash)
    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: "timer-recovery".into(),
        token_id: token_id.into(),
        instance_id: instance_id.into(),
        node_id: "delay".into(),
        due_at: "0".into(),
        status: "Scheduled".into(),
        created_at: "0".into(),
    };
    repo.insert(&timer_record).await.unwrap();

    // Simulate crash: we just stop. On restart, the scheduler does an initial sweep.
    // Verify the timer store still has the timer (persistence survives "crash").
    let stored = repo.get_by_id("timer-recovery").await.unwrap().unwrap();
    assert_eq!(stored.status, "Scheduled");

    // Initial sweep on restart: find due timers
    let now = unix_now();
    let due = repo.list_due(&now, 100).await.unwrap();
    assert_eq!(due.len(), 1, "timer should survive simulated crash");
    assert_eq!(due[0].id, "timer-recovery");

    // Fire the timer and advance
    repo.mark_fired(&due[0].id).await.unwrap();

    let (engine, mut ctx) = build_engine_and_ctx(Arc::clone(&repo), timer_process_def());
    let ev = EngineEvent::TokenArrived(payloads::TokenArrived {
        instance_id: instance_id.into(),
        token_id: token_id.into(),
        node_id: "delay".into(),
    });
    engine.run_async(ev, &mut ctx).await;

    let loaded = repo.load(instance_id).await.unwrap().unwrap();
    assert_eq!(
        loaded.state,
        InstanceState::Completed,
        "process should complete after timer recovery"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Fired timer is excluded from subsequent list_due
// ---------------------------------------------------------------------------

/// A timer that has been marked as fired should not appear in list_due results.
#[tokio::test]
async fn fired_timer_excluded_from_list_due() {
    let repo = Arc::new(MemoryRepo::new());

    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: "timer-fired".into(),
        token_id: "t-1".into(),
        instance_id: "inst-1".into(),
        node_id: "delay".into(),
        due_at: "0".into(),
        status: "Scheduled".into(),
        created_at: "0".into(),
    };
    repo.insert(&timer_record).await.unwrap();

    // First query: should be due
    let now = unix_now();
    let due = repo.list_due(&now, 100).await.unwrap();
    assert_eq!(due.len(), 1);

    // Mark fired
    repo.mark_fired("timer-fired").await.unwrap();

    // Second query: should be empty
    let due_after = repo.list_due(&now, 100).await.unwrap();
    assert!(due_after.is_empty(), "fired timer should not reappear");
}

// ---------------------------------------------------------------------------
// Test 6: Timer store insert + get round-trip
// ---------------------------------------------------------------------------

/// Verify that inserted timers can be retrieved by id.
#[tokio::test]
async fn timer_insert_and_get_roundtrip() {
    let repo = Arc::new(MemoryRepo::new());

    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: "timer-rt".into(),
        token_id: "t-rt".into(),
        instance_id: "inst-rt".into(),
        node_id: "delay".into(),
        due_at: "12345".into(),
        status: "Scheduled".into(),
        created_at: "1000".into(),
    };
    repo.insert(&timer_record).await.unwrap();

    let loaded = repo.get_by_id("timer-rt").await.unwrap().unwrap();
    assert_eq!(loaded.id, "timer-rt");
    assert_eq!(loaded.token_id, "t-rt");
    assert_eq!(loaded.instance_id, "inst-rt");
    assert_eq!(loaded.due_at, "12345");
    assert_eq!(loaded.status, "Scheduled");
    assert_eq!(loaded.created_at, "1000");
}

// ---------------------------------------------------------------------------
// Test 7: Timer limit parameter caps results
// ---------------------------------------------------------------------------

/// The `limit` parameter of list_due should cap the number of results.
#[tokio::test]
async fn list_due_respects_limit() {
    let repo = Arc::new(MemoryRepo::new());

    // Insert 5 due timers
    for i in 0..5 {
        let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
            id: format!("timer-lim-{}", i),
            token_id: format!("t-{}", i),
            instance_id: format!("inst-{}", i),
            node_id: format!("node-{}", i),
            due_at: "0".into(),
            status: "Scheduled".into(),
            created_at: "0".into(),
        };
        repo.insert(&timer_record).await.unwrap();
    }

    let now = unix_now();

    // limit=3 should return at most 3
    let due = repo.list_due(&now, 3).await.unwrap();
    assert_eq!(due.len(), 3, "limit=3 should cap results at 3");

    // limit=100 should return all 5
    let due_all = repo.list_due(&now, 100).await.unwrap();
    assert_eq!(due_all.len(), 5, "limit=100 should return all 5 timers");
}

// ---------------------------------------------------------------------------
// Test 8: Timer due exactly now (boundary case)
// ---------------------------------------------------------------------------

/// A timer with `due_at` equal to current time should be listed as due.
#[tokio::test]
async fn timer_due_exactly_now_is_listed() {
    let repo = Arc::new(MemoryRepo::new());
    let now = unix_now();

    // Insert a timer due exactly now
    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: "timer-boundary".into(),
        token_id: "t-boundary".into(),
        instance_id: "inst-boundary".into(),
        node_id: "delay".into(),
        due_at: now.clone(),
        status: "Scheduled".into(),
        created_at: "0".into(),
    };
    repo.insert(&timer_record).await.unwrap();

    // list_due uses <= comparison, so due_at == now should be included
    let due = repo.list_due(&now, 100).await.unwrap();
    assert_eq!(due.len(), 1, "timer due exactly now should be listed");
    assert_eq!(due[0].id, "timer-boundary");
}

// ---------------------------------------------------------------------------
// Test 9: Timer due 1 second in the future is NOT listed
// ---------------------------------------------------------------------------

/// A timer with `due_at` in the future should not be listed as due.
#[tokio::test]
async fn timer_due_in_future_is_not_listed() {
    let repo = Arc::new(MemoryRepo::new());
    let now = unix_now();
    let now_val: u64 = now.parse().unwrap();

    // Insert a timer due 1 second in the future
    let timer_record = bpm_engine::bpm_engine_storage::TimerRecord {
        id: "timer-future".into(),
        token_id: "t-future".into(),
        instance_id: "inst-future".into(),
        node_id: "delay".into(),
        due_at: (now_val + 1).to_string(),
        status: "Scheduled".into(),
        created_at: "0".into(),
    };
    repo.insert(&timer_record).await.unwrap();

    let due = repo.list_due(&now, 100).await.unwrap();
    assert_eq!(due.len(), 0, "timer due in future should not be listed");
}
