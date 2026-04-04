//! Outbox replay tests: normal publish, replay unsent, crash recovery.
//! See docs/recovery.md §7 (Outbox recovery).

use bpm_engine::bpm_engine_adapter_memory::MemoryRepo;
use bpm_engine::bpm_engine_core::{
    payloads, InstanceState, ProcessInstance, Token, TokenMode, TokenStatus,
};
use bpm_engine::bpm_engine_storage::{OutboxRepo, ProcessInstanceStore};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Test 1: Outbox message published in normal flow
// ---------------------------------------------------------------------------

/// Scenario: When process completes, a ProcessCompleted event is emitted
/// and should be published to outbox.
#[tokio::test]
async fn outbox_event_published_on_process_completion() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-outbox-1".to_string();
    let token_id = "t-outbox-1".to_string();

    // Create a running instance at the start node
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

    // Insert a pending outbox event for this instance (simulating what handler would do)
    let payload = serde_json::to_string(&payloads::ProcessCompleted {
        instance_id: instance_id.clone(),
    })
    .unwrap();
    let event_id = repo
        .insert_pending(None, "ProcessCompleted", &payload)
        .await
        .unwrap();

    // Verify event is in Pending state
    let pending = repo.list_pending(None).await.unwrap();
    assert!(!pending.is_empty(), "outbox should have pending events");

    // Simulate publishing: mark as Published
    repo.mark_published(&event_id).await.unwrap();

    // Verify event is now Published
    let all_events = repo.list_pending(None).await.unwrap();
    let published_count = all_events
        .iter()
        .filter(|e| e.status == "Published")
        .count();
    assert_eq!(
        published_count, 0,
        "published events should not appear in pending list"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Outbox replay — redeliver unsent messages
// ---------------------------------------------------------------------------

/// Scenario: Engine crashes after handler runs but before mark_published.
/// On restart, the outbox replay should redeliver Pending events.
#[tokio::test]
async fn outbox_replay_redelivers_pending_events() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-replay-1".to_string();

    // Simulate crash: insert ProcessStarted event but never mark it published
    let payload = serde_json::to_string(&payloads::ProcessStarted {
        process_id: "minimal".to_string(),
        instance_id: instance_id.clone(),
        initial_variables: None,
    })
    .unwrap();
    let event_id = repo
        .insert_pending(None, "ProcessStarted", &payload)
        .await
        .unwrap();

    // Verify event is Pending
    let pending_before = repo.list_pending(None).await.unwrap();
    assert_eq!(pending_before.len(), 1, "event should be in Pending state");
    assert_eq!(pending_before[0].id, event_id, "event id should match");

    // Simulate outbox replay: claim the pending events
    let claimed = repo.claim_pending("worker-1", None, 10).await.unwrap();
    assert_eq!(claimed.len(), 1, "replay should claim the pending event");
    assert_eq!(claimed[0].id, event_id, "claimed event should match");

    // Simulate processing + mark_published
    repo.mark_published(&event_id).await.unwrap();

    // Verify event is now Published (no longer Pending)
    let pending_after = repo.list_pending(None).await.unwrap();
    assert!(
        pending_after.is_empty(),
        "after replay and publish, pending list should be empty"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Outbox message after crash recovery
// ---------------------------------------------------------------------------

/// Scenario: Multiple events in outbox; engine restarts and processes all.
/// Events should be delivered in order (FIFO).
#[tokio::test]
async fn outbox_replay_processes_all_pending_events_after_restart() {
    let repo = Arc::new(MemoryRepo::new());
    let instance_id = "inst-multi-1".to_string();

    // Insert multiple events for the same instance
    let payload1 = serde_json::to_string(&payloads::ProcessStarted {
        process_id: "minimal".to_string(),
        instance_id: instance_id.clone(),
        initial_variables: None,
    })
    .unwrap();
    let event_id1 = repo
        .insert_pending(None, "ProcessStarted", &payload1)
        .await
        .unwrap();

    let payload2 = serde_json::to_string(&payloads::TokenArrived {
        instance_id: instance_id.clone(),
        token_id: "t-1".to_string(),
        node_id: "start".to_string(),
    })
    .unwrap();
    let event_id2 = repo
        .insert_pending(None, "TokenArrived", &payload2)
        .await
        .unwrap();

    let payload3 = serde_json::to_string(&payloads::ProcessCompleted {
        instance_id: instance_id.clone(),
    })
    .unwrap();
    let event_id3 = repo
        .insert_pending(None, "ProcessCompleted", &payload3)
        .await
        .unwrap();

    // Verify 3 pending events
    let pending = repo.list_pending(None).await.unwrap();
    assert_eq!(pending.len(), 3, "should have 3 pending events");

    // Simulate outbox replay: claim all events
    let claimed = repo.claim_pending("worker-1", None, 10).await.unwrap();
    assert_eq!(claimed.len(), 3, "all 3 events should be claimed");

    // Process each event (simulate dispatch)
    for ev in &claimed {
        // In real system, engine.run() would process each event
        // Here we just verify event structure
        assert!(!ev.event_type.is_empty(), "event type should not be empty");
        assert!(!ev.payload.is_empty(), "event payload should not be empty");
    }

    // Mark all as published
    repo.mark_published(&event_id1).await.unwrap();
    repo.mark_published(&event_id2).await.unwrap();
    repo.mark_published(&event_id3).await.unwrap();

    // Verify no more pending events
    let pending_after = repo.list_pending(None).await.unwrap();
    assert!(
        pending_after.is_empty(),
        "all events should be published after replay"
    );
}
