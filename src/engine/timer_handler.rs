//! Timer model and TimerFiredHandler stub (design: timer.md).
//! Timer table and Timer Poller to be implemented when integrating.

use crate::engine::events::{payloads, EngineEvent};
use crate::engine::handler::{EngineContext, EventHandler};

/// Timer type (design: timer.md §2.1).
#[derive(Debug, Clone)]
pub enum TimerType {
    Delay,
    Timeout,
    RetryBackoff,
}

/// Timer record (design: timer.md §2.1).
#[derive(Debug)]
pub struct Timer {
    pub id: String,
    pub token_id: String,
    pub fire_at: String,
    pub timer_type: TimerType,
    pub status: String,
}

/// TimerFiredHandler: on TimerFired, unblock token and emit TokenArrived (design: timer.md §7).
/// Stub: returns empty until TimerRepo and token unblock are wired.
pub struct TimerFiredHandler;

impl EventHandler for TimerFiredHandler {
    fn handle(&self, event: &EngineEvent, _ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TimerFired(_e) = event else {
            return vec![];
        };
        // TODO: load timer, load token, unblock, save, emit TokenArrived
        vec![]
    }
}
