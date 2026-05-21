//! Replay session: apply history events one-by-one to produce a read-only snapshot.

use bpm_engine_core::{EngineEvent, ProcessInstance};
use bpm_engine_storage::HistoryEvent;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

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

/// Session TTL: 30 minutes.
const SESSION_TTL_SECS: u64 = 1800;
/// Maximum concurrent sessions.
const MAX_SESSIONS: usize = 100;

pub struct ReplaySession {
    #[allow(dead_code)]
    pub instance_id: String,
    pub events: Vec<HistoryEvent>,
    pub cursor: usize,
    pub snapshot: Option<ProcessInstance>,
    pub last_accessed: Instant,
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
            last_accessed: Instant::now(),
        }
    }

    pub fn total_events(&self) -> usize {
        self.events.len()
    }

    pub fn current_event(&self) -> Option<&HistoryEvent> {
        self.events.get(self.cursor)
    }

    pub fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }

    pub fn parse_event(ev: &HistoryEvent) -> Option<EngineEvent> {
        let payload_str = serde_json::to_string(&ev.payload).ok()?;
        bpm_engine_core::event_from_outbox(ev.event_type.as_str(), &payload_str)
    }
}

/// Shared map of session_id -> ReplaySession with TTL eviction.
pub type ReplaySessions = RwLock<HashMap<String, ReplaySession>>;

/// Evict expired sessions and enforce max capacity.
pub fn evict_expired(sessions: &ReplaySessions) {
    let Ok(mut guard) = sessions.write() else {
        return;
    };
    let now = Instant::now();
    guard.retain(|_, session| {
        now.duration_since(session.last_accessed).as_secs() < SESSION_TTL_SECS
    });
    // If still over capacity, remove oldest
    while guard.len() > MAX_SESSIONS {
        let oldest_key = guard
            .iter()
            .min_by_key(|(_, s)| s.last_accessed)
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest_key {
            guard.remove(&key);
        } else {
            break;
        }
    }
}
