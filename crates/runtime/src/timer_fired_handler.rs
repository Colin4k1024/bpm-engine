//! TimerFiredHandler: handles TimerFired events for boundary events and timer catch events.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent, NodeType, TokenStatus};
use tracing::{debug, info, warn};

use super::handler::{EngineContext, EventHandler};
use super::transition::move_token;

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
