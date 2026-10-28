//! TimerFiredHandler: handles TimerFired events for boundary events and timer catch events.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent, NodeType, TokenStatus};
use tracing::{debug, info, warn};

use super::handler::{EngineContext, EventHandler};
use super::transition::move_token;

/// How long (seconds) a timer on a suspended (paused) host token is
/// re-postponed before it fires (and re-reschedules) again — a short poll
/// interval, so the timer fires promptly once resume un-suspends the token
/// (E5-2 / ADR-008 §2 freeze semantics: rescheduled, never dropped).
const RESCHEDULLED_POLL_INTERVAL_SECS: u64 = 1;

/// Handler that advances tokens when a timer fires (boundary events and timer catch events).
pub struct TimerFiredHandler;

#[async_trait]
impl EventHandler for TimerFiredHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TimerFired(e) = event else {
            return vec![];
        };

        // Look up the timer record to get instance_id and node_id
        let timer_store = match &ctx.timer_store {
            Some(store) => store,
            None => return vec![],
        };
        let timer_record = match timer_store.get_by_id(&e.timer_id).await {
            Ok(Some(r)) => r,
            _ => return vec![],
        };

        let instance_id = &timer_record.instance_id;
        let node_id = &timer_record.node_id;

        if node_id.is_empty() {
            debug!(timer_id = %e.timer_id, "timer has no node_id, skipping");
            return vec![];
        }

        let process_store = &ctx.process_store;
        let process_def_store = &ctx.process_def_store;

        let Ok(Some(mut instance)) = process_store.load(instance_id).await else {
            return vec![];
        };
        let Ok(Some(def)) = process_def_store.load(&instance.process_def_id).await else {
            return vec![];
        };

        let Some(node) = def.nodes.get(node_id.as_str()) else {
            return vec![];
        };

        let mut out = vec![];

        // E5-2 pause (ADR-008 §2): a timer whose host token is Suspended is
        // FROZEN, not consumed. The scheduler already marked the record
        // Fired before dispatching this event, so a plain early return
        // would LOSE the deadline forever. Instead, RESCHEDULE the same
        // timer record (id re-insert, ADR-004): due_at = now + a short poll
        // interval, status = Scheduled — it re-fires (and re-reschedules)
        // until the token is un-suspended, and fires normally once resume
        // has restored the catch/host token. Boundary branches therefore
        // never fire while paused; the deadline survives the pause.
        if instance
            .tokens
            .iter()
            .any(|t| t.id == e.token_id && t.status == TokenStatus::Suspended)
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut rescheduled = timer_record.clone();
            rescheduled.status = "Scheduled".to_string();
            rescheduled.due_at = (now + RESCHEDULLED_POLL_INTERVAL_SECS).to_string();
            match timer_store.insert(&rescheduled).await {
                Ok(()) => {
                    debug!(
                        instance_id = %instance_id,
                        timer_id = %e.timer_id,
                        due_in_secs = RESCHEDULLED_POLL_INTERVAL_SECS,
                        "timer fired on a suspended (paused) host token; rescheduled \
                         (frozen) until resume"
                    );
                }
                Err(err) => {
                    warn!(
                        instance_id = %instance_id,
                        timer_id = %e.timer_id,
                        error = %err,
                        "failed to reschedule a timer on a suspended host token; \
                         the deadline is lost for this fire"
                    );
                }
            }
            return vec![];
        }

        match &node.node_type {
            NodeType::BoundaryTimer {
                is_interrupting, ..
            } => {
                info!(
                    instance_id = %instance_id,
                    node_id = %node_id,
                    is_interrupting = is_interrupting,
                    "boundary timer fired"
                );
                if *is_interrupting {
                    // Remove the host token (find by token_id from timer)
                    let host_token_pos = instance
                        .tokens
                        .iter()
                        .position(|t| t.id == e.token_id && t.status == TokenStatus::Waiting);
                    if let Some(pos) = host_token_pos {
                        instance.tokens.remove(pos);
                    }
                    // Create new token at the boundary event's target
                    let new_tokens = move_token(node);
                    for t in &new_tokens {
                        out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                            instance_id: instance_id.clone(),
                            token_id: t.id.clone(),
                            node_id: t.node_id.clone(),
                        }));
                    }
                    instance.tokens.extend(new_tokens);
                } else {
                    // Non-interrupting: create additional token at target
                    let new_tokens = move_token(node);
                    for t in &new_tokens {
                        out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                            instance_id: instance_id.clone(),
                            token_id: t.id.clone(),
                            node_id: t.node_id.clone(),
                        }));
                    }
                    instance.tokens.extend(new_tokens);
                }
                let _ = process_store.save(&instance).await;
            }
            NodeType::TimerIntermediateCatch { .. } => {
                info!(
                    instance_id = %instance_id,
                    node_id = %node_id,
                    "timer intermediate catch event fired"
                );
                // Find the waiting token at this node
                let token_pos = instance
                    .tokens
                    .iter()
                    .position(|t| t.node_id.as_str() == node_id.as_str() && t.waiting());
                if let Some(pos) = token_pos {
                    instance.tokens.remove(pos);
                    let new_tokens = move_token(node);
                    for t in &new_tokens {
                        out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                            instance_id: instance_id.clone(),
                            token_id: t.id.clone(),
                            node_id: t.node_id.clone(),
                        }));
                    }
                    instance.tokens.extend(new_tokens);
                    let _ = process_store.save(&instance).await;
                }
            }
            _ => {
                warn!(
                    timer_id = %e.timer_id,
                    node_id = %node_id,
                    "timer fired for non-timer node type"
                );
            }
        }

        out
    }
}
