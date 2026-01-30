//! Timer model and TimerFiredHandler (design: timer.md).
//! On TimerFired: load timer, unblock token (Waiting -> Ready), save, emit TokenArrived.

use crate::engine::events::{payloads, EngineEvent};
use crate::engine::handler::{EngineContext, EventHandler};
use crate::model::TokenStatus;

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
pub struct TimerFiredHandler;

impl EventHandler for TimerFiredHandler {
    fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TimerFired(e) = event else {
            return vec![];
        };
        let Some(ref timer_repo) = ctx.timer_repo else {
            return vec![];
        };
        let Some(ref process_repo) = ctx.process_repo else {
            return vec![];
        };
        let Some(ref token_repo) = ctx.token_repo else {
            return vec![];
        };

        let Some(timer) = timer_repo.get_by_id(&e.timer_id) else {
            return vec![];
        };
        if timer.status != "Scheduled" {
            return vec![];
        }
        if let Err(_) = timer_repo.mark_fired(&e.timer_id) {
            return vec![];
        }

        let Some(mut instance) = process_repo.load(&timer.instance_id) else {
            return vec![];
        };
        let token_idx = instance.tokens.iter().position(|t| t.id == timer.token_id);
        let Some(idx) = token_idx else {
            return vec![];
        };
        if instance.tokens[idx].status != TokenStatus::Waiting {
            return vec![];
        }
        let node_id = instance.tokens[idx].node_id.clone();
        instance.tokens[idx].status = TokenStatus::Ready;
        process_repo.save(&instance);
        token_repo.save_tokens(&instance.id, &instance.tokens);

        vec![EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id: timer.instance_id,
            token_id: timer.token_id,
            node_id,
        })]
    }
}
