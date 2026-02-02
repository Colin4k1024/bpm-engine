//! Replay session: apply history events one-by-one to produce a read-only snapshot (design: docs_replay_rest_api.md).

use bpm_engine_core::{EngineEvent, ProcessInstance};
use bpm_engine_storage::HistoryEvent;
use std::sync::RwLock;

/// Event types that we can replay (parse and apply via engine). External task lifecycle events are trace-only.
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

/// In-memory replay session: events (filtered to replayable), cursor (next index to apply), snapshot after applying [0..cursor).
pub struct ReplaySession {
    #[allow(dead_code)]
    pub instance_id: String,
    pub events: Vec<HistoryEvent>,
    pub cursor: usize,
    pub snapshot: Option<ProcessInstance>,
}

impl ReplaySession {
    pub fn new(instance_id: String, events: Vec<HistoryEvent>) -> Self {
        let events: Vec<HistoryEvent> = events
            .into_iter()
            .filter(|e| is_replayable(e.event_type.as_str()))
            .collect();
        ReplaySession {
            instance_id,
            events,
            cursor: 0,
            snapshot: None,
        }
    }

    pub fn total_events(&self) -> usize {
        self.events.len()
    }

    /// Current event at cursor (the one that would be applied on next step), if any.
    pub fn current_event(&self) -> Option<&HistoryEvent> {
        self.events.get(self.cursor)
    }

    /// Parse HistoryEvent to EngineEvent for replay. Returns None if event type is not replayable or payload fails to parse.
    pub fn parse_event(ev: &HistoryEvent) -> Option<EngineEvent> {
        let payload_str = serde_json::to_string(&ev.payload).ok()?;
        bpm_engine_core::event_from_outbox(ev.event_type.as_str(), &payload_str)
    }
}

/// Shared map of session_id -> ReplaySession. Sessions are ephemeral (not persisted).
pub type ReplaySessions = RwLock<std::collections::HashMap<String, ReplaySession>>;
