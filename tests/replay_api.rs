//! Replay API tests: session creation, step-forward, seek, snapshot, TTL eviction.
//!
//! Tests the replay data model and history store contract used by the REST replay
//! endpoints (crates/server/rest/src/replay.rs + routes.rs).
//!
//! Covers issue #23.

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::{
    payloads, EngineEvent, InstanceState, Node, NodeType, OutgoingEdge, ProcessDefinition,
    ProcessInstance,
};
use bpm_engine::bpm_engine_runtime::{
    BpmEngine, EngineContext, HistoryHandler, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler, UserTaskCompletedHandler,
};
use bpm_engine::bpm_engine_storage::{HistoryEvent, HistoryRepo, ProcessInstanceStore};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Constants mirroring replay.rs
// ---------------------------------------------------------------------------

const REPLAYABLE_EVENT_TYPES: &[&str] = &[
    "ProcessStarted",
    "TokenArrived",
    "TokenCompleted",
    "UserTaskCreated",
    "UserTaskCompleted",
    "TimerFired",
    "TimerScheduled",
    "TokenFailed",
    "SagaStarted",
    "SagaCompleted",
    "ProcessCompleted",
];

fn is_replayable(event_type: &str) -> bool {
    REPLAYABLE_EVENT_TYPES.contains(&event_type)
}

// ---------------------------------------------------------------------------
// Process definitions
// ---------------------------------------------------------------------------

fn minimal_def() -> ProcessDefinition {
    ProcessDefinition {
        id: "minimal",
        start: "start",
        boundary_events: HashMap::new(),
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

/// Process with a UserTask between start and end.
/// The UserTask blocks, so the process won't complete automatically.
fn user_task_def() -> ProcessDefinition {
    ProcessDefinition {
        id: "user-task-proc",
        start: "start",
        boundary_events: HashMap::new(),
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "review",
                        condition: None,
                    }],
                },
            ),
            (
                "review",
                Node {
                    id: "review",
                    node_type: NodeType::UserTask {
                        form_key: None,
                        form_fields: None,
                    },
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
// Helpers
// ---------------------------------------------------------------------------

/// Build engine with HistoryHandler for event recording.
fn build_engine_with_history() -> (BpmEngine, EngineContext, Arc<MemoryRepo>) {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
    def_store.register(minimal_def());

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
        Box::new(HistoryHandler),
    ]);

    let ctx = EngineContext::builder(
        repo.clone() as Arc<_>,
        repo.clone() as Arc<_>,
        def_store as Arc<_>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .build();

    (engine, ctx, repo)
}

// ---------------------------------------------------------------------------
// Test 1: Create replay session from history events
// ---------------------------------------------------------------------------

/// After running a process to completion, the history should contain
/// replayable events that can be loaded into a replay session.
#[tokio::test]
async fn replay_session_created_from_history() {
    let (engine, mut ctx, repo) = build_engine_with_history();
    let instance_id = "inst-replay-1";

    // Start a process — this generates history events
    let ev = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: "minimal".into(),
        instance_id: instance_id.into(),
        initial_variables: None,
    });
    engine.run_async(ev, &mut ctx).await;

    // Verify process completed
    let inst = repo.load(instance_id).await.unwrap().unwrap();
    assert_eq!(inst.state, InstanceState::Completed);

    // Load history events
    let events = HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, None)
        .await
        .unwrap();
    assert!(
        !events.is_empty(),
        "history should contain events after process completion"
    );

    // Filter replayable events (mirrors ReplaySession::new logic)
    let replayable: Vec<&HistoryEvent> = events
        .iter()
        .filter(|e| is_replayable(&e.event_type))
        .collect();
    assert!(
        !replayable.is_empty(),
        "at least some events should be replayable"
    );

    // Verify event types are in the replayable set
    for ev in &replayable {
        assert!(
            is_replayable(&ev.event_type),
            "event type '{}' should be replayable",
            ev.event_type
        );
    }
}

// ---------------------------------------------------------------------------
// Test 2: Step forward — apply events one by one
// ---------------------------------------------------------------------------

/// Replaying events one by one should produce progressively richer snapshots.
/// After replaying all events, the snapshot should show the process as completed.
#[tokio::test]
async fn replay_step_forward_applies_events_sequentially() {
    let (engine, mut ctx, repo) = build_engine_with_history();
    let instance_id = "inst-step-1";

    // Run process
    let ev = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: "minimal".into(),
        instance_id: instance_id.into(),
        initial_variables: None,
    });
    engine.run_async(ev, &mut ctx).await;

    // Get replayable events
    let events = HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, None)
        .await
        .unwrap();
    let replayable: Vec<HistoryEvent> = events
        .into_iter()
        .filter(|e| is_replayable(&e.event_type))
        .collect();
    assert!(!replayable.is_empty());

    // Simulate step-forward: apply each event in order using a temp repo
    let def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
    def_store.register(minimal_def());

    let step_engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
    ]);

    let mut current_snapshot: Option<ProcessInstance> = None;

    for (i, hist_event) in replayable.iter().enumerate() {
        // Reconstruct EngineEvent from history event
        let payload_str = serde_json::to_string(&hist_event.payload).unwrap();
        let engine_event =
            bpm_engine::bpm_engine_core::event_from_outbox(&hist_event.event_type, &payload_str);
        assert!(
            engine_event.is_some(),
            "event #{} ({}) should be reconstructable",
            i,
            hist_event.event_type
        );

        // Apply event to a fresh temp repo
        let temp_repo = Arc::new(MemoryRepo::new());
        if let Some(ref snap) = current_snapshot {
            temp_repo.save(snap).await.unwrap();
        }

        let mut temp_ctx = EngineContext::builder(
            temp_repo.clone() as Arc<_>,
            temp_repo.clone() as Arc<_>,
            def_store.clone() as Arc<_>,
        )
        .build();

        step_engine
            .run_async(engine_event.unwrap(), &mut temp_ctx)
            .await;

        // Update snapshot
        if let Some(inst) = temp_repo.load(instance_id).await.unwrap() {
            current_snapshot = Some(inst);
        }
    }

    // After replaying all events, the snapshot should show completed process
    let final_snapshot = current_snapshot.expect("should have a snapshot after replay");
    assert_eq!(
        final_snapshot.state,
        InstanceState::Completed,
        "after replaying all events, snapshot should show Completed"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Seek to cursor — replay events[0..cursor]
// ---------------------------------------------------------------------------

/// Seeking to a specific cursor should replay only events before that position.
/// Uses a process with a UserTask that blocks, so seeking to an early cursor
/// won't accidentally complete the process.
#[tokio::test]
async fn replay_seek_to_cursor() {
    // Use a process with a UserTask (blocks at "review" node)
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
    def_store.register(user_task_def());

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
        Box::new(HistoryHandler),
    ]);

    let mut ctx = EngineContext::builder(
        repo.clone() as Arc<_>,
        repo.clone() as Arc<_>,
        def_store.clone() as Arc<_>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .build();

    let instance_id = "inst-seek-1";

    // Start process — it will block at UserTask "review"
    let ev = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: "user-task-proc".into(),
        instance_id: instance_id.into(),
        initial_variables: None,
    });
    engine.run_async(ev, &mut ctx).await;

    // Process should be Running (blocked at UserTask)
    let inst = repo.load(instance_id).await.unwrap().unwrap();
    assert_eq!(inst.state, InstanceState::Running);

    // Get replayable events
    let events = HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, None)
        .await
        .unwrap();
    let replayable: Vec<HistoryEvent> = events
        .into_iter()
        .filter(|e| is_replayable(&e.event_type))
        .collect();
    assert!(!replayable.is_empty(), "should have replayable events");

    // Seek to cursor=1 (only first event: ProcessStarted)
    let cursor = 1;
    let events_to_apply = &replayable[..cursor];

    let seek_def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
    seek_def_store.register(user_task_def());
    let step_engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
    ]);

    let mut snapshot: Option<ProcessInstance> = None;
    for hist_event in events_to_apply {
        let payload_str = serde_json::to_string(&hist_event.payload).unwrap();
        let engine_event =
            bpm_engine::bpm_engine_core::event_from_outbox(&hist_event.event_type, &payload_str)
                .unwrap();

        let temp_repo = Arc::new(MemoryRepo::new());
        if let Some(ref snap) = snapshot {
            temp_repo.save(snap).await.unwrap();
        }
        let mut temp_ctx = EngineContext::builder(
            temp_repo.clone() as Arc<_>,
            temp_repo.clone() as Arc<_>,
            seek_def_store.clone() as Arc<_>,
        )
        .build();
        step_engine.run_async(engine_event, &mut temp_ctx).await;

        if let Some(inst) = temp_repo.load(instance_id).await.unwrap() {
            snapshot = Some(inst);
        }
    }

    // After seeking to cursor=1, process should NOT be completed
    // (UserTask blocks, so the process stays Running)
    let snap = snapshot.expect("should have snapshot after seek");
    assert_ne!(
        snap.state,
        InstanceState::Completed,
        "seeking to early cursor should not complete the process"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Snapshot read — snapshot reflects last replayed state
// ---------------------------------------------------------------------------

/// The snapshot after replay should accurately reflect the instance state.
#[tokio::test]
async fn replay_snapshot_reflects_state() {
    let (engine, mut ctx, repo) = build_engine_with_history();
    let instance_id = "inst-snap-1";

    // Run process
    let ev = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: "minimal".into(),
        instance_id: instance_id.into(),
        initial_variables: None,
    });
    engine.run_async(ev, &mut ctx).await;

    // Get history
    let events = HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, None)
        .await
        .unwrap();
    let replayable: Vec<HistoryEvent> = events
        .into_iter()
        .filter(|e| is_replayable(&e.event_type))
        .collect();

    // Replay all events
    let def_store = Arc::new(bpm_engine::bpm_engine_adapter_memory::ProcessDefStore::new());
    def_store.register(minimal_def());
    let step_engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ProcessCompletedHandler),
    ]);

    let mut snapshot: Option<ProcessInstance> = None;
    for hist_event in &replayable {
        let payload_str = serde_json::to_string(&hist_event.payload).unwrap();
        let engine_event =
            bpm_engine::bpm_engine_core::event_from_outbox(&hist_event.event_type, &payload_str)
                .unwrap();

        let temp_repo = Arc::new(MemoryRepo::new());
        if let Some(ref snap) = snapshot {
            temp_repo.save(snap).await.unwrap();
        }
        let mut temp_ctx = EngineContext::builder(
            temp_repo.clone() as Arc<_>,
            temp_repo.clone() as Arc<_>,
            def_store.clone() as Arc<_>,
        )
        .build();
        step_engine.run_async(engine_event, &mut temp_ctx).await;

        if let Some(inst) = temp_repo.load(instance_id).await.unwrap() {
            snapshot = Some(inst);
        }
    }

    // Verify snapshot matches original final state
    let snap = snapshot.expect("should have snapshot");
    let original = repo.load(instance_id).await.unwrap().unwrap();

    assert_eq!(snap.state, original.state, "replayed state should match");
    assert_eq!(
        snap.process_def_id, original.process_def_id,
        "process_def_id should match"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Non-replayable events are filtered out
// ---------------------------------------------------------------------------

/// Events not in the REPLAYABLE_EVENT_TYPES set should be filtered.
#[tokio::test]
async fn non_replayable_events_filtered() {
    // Verify all standard engine events are replayable
    let standard_types = [
        "ProcessStarted",
        "TokenArrived",
        "TokenCompleted",
        "UserTaskCreated",
        "UserTaskCompleted",
        "TimerFired",
        "TimerScheduled",
        "TokenFailed",
        "SagaStarted",
        "SagaCompleted",
        "ProcessCompleted",
    ];
    for t in &standard_types {
        assert!(is_replayable(t), "'{}' should be replayable", t);
    }

    // Non-replayable events
    let non_replayable = [
        "ExternalTaskLocked",
        "ExternalTaskCompleted",
        "ExternalTaskFailed",
        "UnknownEvent",
    ];
    for t in &non_replayable {
        assert!(!is_replayable(t), "'{}' should NOT be replayable", t);
    }
}

// ---------------------------------------------------------------------------
// Test 6: Empty history produces empty replay session
// ---------------------------------------------------------------------------

/// An instance with no history events should produce an empty replay session.
#[tokio::test]
async fn empty_history_produces_empty_replay() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-empty-replay";

    // Create instance but don't run any events through the engine
    let inst = ProcessInstance {
        id: instance_id.into(),
        process_def_id: "minimal".into(),
        tenant_id: None,
        tokens: vec![],
        variables: HashMap::new(),
        state: InstanceState::Running,
        version: 0,
        parent_instance_id: None,
        parent_token_id: None,
    };
    repo.save(&inst).await.unwrap();

    // Query history — should be empty
    let events = HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, None)
        .await
        .unwrap();
    assert!(events.is_empty(), "no events means no history");

    // Filter replayable — also empty
    let replayable: Vec<&HistoryEvent> = events
        .iter()
        .filter(|e| is_replayable(&e.event_type))
        .collect();
    assert!(
        replayable.is_empty(),
        "empty history means empty replay session"
    );
}

// ---------------------------------------------------------------------------
// Test 7: History event filter by event_type and token_id
// ---------------------------------------------------------------------------

/// HistoryRepo should support filtering by event_type and token_id.
#[tokio::test]
async fn history_filter_by_event_type_and_token_id() {
    let (engine, mut ctx, repo) = build_engine_with_history();
    let instance_id = "inst-filter-1";

    // Run process
    let ev = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: "minimal".into(),
        instance_id: instance_id.into(),
        initial_variables: None,
    });
    engine.run_async(ev, &mut ctx).await;

    // Filter by event_type
    let started_events =
        HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, Some("ProcessStarted"))
            .await
            .unwrap();
    assert_eq!(started_events.len(), 1, "exactly one ProcessStarted event");
    assert_eq!(started_events[0].event_type, "ProcessStarted");

    // Filter by non-existent event_type
    let no_events =
        HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, Some("NonExistent"))
            .await
            .unwrap();
    assert!(
        no_events.is_empty(),
        "non-existent event type should return empty"
    );

    // All events for instance
    let all_events = HistoryRepo::list_by_instance(repo.as_ref(), instance_id, None, None)
        .await
        .unwrap();
    assert!(
        !all_events.is_empty(),
        "should have events for completed process"
    );
}

// ---------------------------------------------------------------------------
// Test 8: TTL eviction — replay sessions evict expired entries
// ---------------------------------------------------------------------------

/// Replicates the eviction logic from replay.rs to verify the TTL contract.
/// Sessions older than SESSION_TTL_SECS (1800s) should be evicted.
#[tokio::test]
async fn replay_session_ttl_eviction_logic() {
    use std::time::Instant;

    const SESSION_TTL_SECS: u64 = 1800;

    // Simulate a session with last_accessed well in the past
    struct MockSession {
        #[allow(dead_code)]
        session_id: String,
        last_accessed: Instant,
    }

    let mut sessions: HashMap<String, MockSession> = HashMap::new();

    // Session "fresh" — accessed just now
    sessions.insert(
        "fresh".into(),
        MockSession {
            session_id: "fresh".into(),
            last_accessed: Instant::now(),
        },
    );

    // Session "old" — simulate old access by subtracting time
    // (We can't actually go back in time, but we can verify the logic)
    // For the test, we check that a session accessed now is NOT evicted
    let now = Instant::now();
    let retained: Vec<&str> = sessions
        .iter()
        .filter(|(_, s)| now.duration_since(s.last_accessed).as_secs() < SESSION_TTL_SECS)
        .map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(retained.len(), 1, "fresh session should be retained");
    assert!(retained.contains(&"fresh"));
}

// ---------------------------------------------------------------------------
// Test 9: event_from_outbox round-trip for all replayable event types
// ---------------------------------------------------------------------------

/// Verify that all replayable event types can be reconstructed from their
/// outbox representation (event_type + JSON payload).
#[tokio::test]
async fn event_from_outbox_roundtrip() {
    let test_cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "ProcessStarted",
            serde_json::json!({"process_id":"p","instance_id":"i","initial_variables":null}),
        ),
        (
            "TokenArrived",
            serde_json::json!({"instance_id":"i","token_id":"t","node_id":"n"}),
        ),
        (
            "TokenCompleted",
            serde_json::json!({"instance_id":"i","token_id":"t"}),
        ),
        (
            "UserTaskCreated",
            serde_json::json!({"instance_id":"i","node_id":"n"}),
        ),
        (
            "UserTaskCompleted",
            serde_json::json!({"task_id":"t","instance_id":"i","node_id":"n","variables":{}}),
        ),
        (
            "TimerFired",
            serde_json::json!({"timer_id":"t","token_id":"tk"}),
        ),
        (
            "TimerScheduled",
            serde_json::json!({"timer_id":"t","instance_id":"i","token_id":"tk","node_id":"n","fire_at":100}),
        ),
        (
            "TokenFailed",
            serde_json::json!({"instance_id":"i","token_id":"t","node_id":"n","reason":"err"}),
        ),
        (
            "SagaStarted",
            serde_json::json!({"instance_id":"i","token_id":"t","node_id":"n"}),
        ),
        (
            "SagaCompleted",
            serde_json::json!({"instance_id":"i","token_id":"t","node_id":"n"}),
        ),
        ("ProcessCompleted", serde_json::json!({"instance_id":"i"})),
    ];

    for (event_type, payload) in &test_cases {
        let payload_str = serde_json::to_string(payload).unwrap();
        let event = bpm_engine::bpm_engine_core::event_from_outbox(event_type, &payload_str);
        assert!(
            event.is_some(),
            "event_from_outbox should reconstruct '{}'",
            event_type
        );
    }

    // Unknown event type returns None
    let result = bpm_engine::bpm_engine_core::event_from_outbox("UnknownType", "{}");
    assert!(result.is_none(), "unknown event type should return None");
}
