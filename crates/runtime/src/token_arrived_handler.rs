//! TokenArrivedHandler: TokenArrived -> advance token, emit TokenArrived/ProcessCompleted.
//! Handles ParallelFork / ParallelJoin.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent, InstanceState, NodeType, TokenStatus};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tracing::{debug, info_span, warn};

use super::handler::{EngineContext, EventHandler};
use super::transition::{
    evaluate_exclusive_gateway, move_token, move_token_preserving_group, move_token_with_group,
};

/// Main handler for TokenArrived events — executes node logic and advances tokens.
pub struct TokenArrivedHandler {
    join_state: Mutex<HashMap<String, (usize, HashSet<String>)>>,
}

impl TokenArrivedHandler {
    /// Create a new handler with empty join state.
    pub fn new() -> Self {
        TokenArrivedHandler {
            join_state: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for TokenArrivedHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventHandler for TokenArrivedHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TokenArrived(e) = event else {
            return vec![];
        };
        let span = info_span!(
            "token.transition",
            instance_id = %e.instance_id,
            token_id = %e.token_id,
            node_id = %e.node_id,
        );
        let _guard = span.enter();
        let process_store = &ctx.process_store;
        drop(_guard);
        let process_def_store = &ctx.process_def_store;
        let Ok(Some(mut instance)) = process_store.load(&e.instance_id).await else {
            return vec![];
        };
        let Ok(Some(def)) = process_def_store.load(&instance.process_def_id).await else {
            return vec![];
        };
        let Some(token_idx) = instance.tokens.iter().position(|t| t.id == e.token_id) else {
            return vec![];
        };
        // Extract group_id early to avoid borrow conflicts
        let parallel_group_id = instance.tokens[token_idx].parallel_group_id.clone();
        let token_store = &ctx.token_store;
        if !token_store
            .claim_token(
                &e.instance_id,
                &e.token_id,
                instance.tokens[token_idx].version,
            )
            .await
            .unwrap_or(false)
        {
            warn!(instance_id = %e.instance_id, token_id = %e.token_id, "token claim failed (CAS)");
            return vec![];
        }
        instance.tokens[token_idx].status = TokenStatus::Executing;
        instance.tokens[token_idx].version += 1;
        debug!(instance_id = %e.instance_id, token_id = %e.token_id, node_id = %e.node_id, "token arrived");
        let node = match def.nodes.get(e.node_id.as_str()) {
            Some(n) => n,
            None => return vec![],
        };

        let mut out = vec![];

        match &node.node_type {
            NodeType::Start => {
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token(node);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::ServiceTask(service) => {
                service(&mut instance);
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token_preserving_group(node, parallel_group_id.clone());
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::UserTask { .. } => {
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                // Schedule boundary event timers for this host node
                if let Some(boundary_defs) = def.boundary_events.get(e.node_id.as_str()) {
                    for bdef in boundary_defs {
                        if let Some(bnode) = def.nodes.get(bdef.node_id) {
                            if let NodeType::BoundaryTimer { duration, .. } = &bnode.node_type {
                                if let Some(ref timer_store) = ctx.timer_store {
                                    let fire_at = parse_duration_to_epoch(duration);
                                    let timer_id = uuid::Uuid::new_v4().to_string();
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs()
                                        .to_string();
                                    let record = bpm_engine_storage::TimerRecord {
                                        id: timer_id,
                                        token_id: e.token_id.clone(),
                                        instance_id: e.instance_id.clone(),
                                        node_id: bdef.node_id.to_string(),
                                        due_at: fire_at.to_string(),
                                        status: "Scheduled".to_string(),
                                        created_at: now,
                                    };
                                    let _ = timer_store.insert(&record).await;
                                }
                            }
                        }
                    }
                }
            }
            NodeType::ExternalTask {
                task_type,
                retries,
                timeout_secs,
            } => {
                if let Some(ref ext_store) = ctx.external_task_store {
                    let variables = instance.variables.clone();
                    let _ = ext_store
                        .create(
                            &e.token_id,
                            &e.instance_id,
                            task_type,
                            *retries,
                            *timeout_secs,
                            variables,
                        )
                        .await;
                }
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                // Schedule boundary event timers for this host node
                if let Some(boundary_defs) = def.boundary_events.get(e.node_id.as_str()) {
                    for bdef in boundary_defs {
                        if let Some(bnode) = def.nodes.get(bdef.node_id) {
                            if let NodeType::BoundaryTimer { duration, .. } = &bnode.node_type {
                                if let Some(ref timer_store) = ctx.timer_store {
                                    let fire_at = parse_duration_to_epoch(duration);
                                    let timer_id = uuid::Uuid::new_v4().to_string();
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs()
                                        .to_string();
                                    let record = bpm_engine_storage::TimerRecord {
                                        id: timer_id,
                                        token_id: e.token_id.clone(),
                                        instance_id: e.instance_id.clone(),
                                        node_id: bdef.node_id.to_string(),
                                        due_at: fire_at.to_string(),
                                        status: "Scheduled".to_string(),
                                        created_at: now,
                                    };
                                    let _ = timer_store.insert(&record).await;
                                }
                            }
                        }
                    }
                }
            }
            NodeType::ExclusiveGateway => {
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                if let Some(t) = evaluate_exclusive_gateway(node, &instance.variables) {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                    instance.tokens.push(t);
                }
            }
            NodeType::End => {
                instance.state = InstanceState::Completed;
                instance.tokens.remove(token_idx);
                let _ = process_store.save(&instance).await;
                return vec![EngineEvent::ProcessCompleted(payloads::ProcessCompleted {
                    instance_id: e.instance_id.clone(),
                })];
            }
            NodeType::ParallelFork => {
                let group_id = uuid::Uuid::new_v4().to_string();
                if let Some(ref join_repo) = ctx.parallel_join_repo {
                    let expected = node.outgoing_edges.len() as u32;
                    let _ = join_repo.ensure_group(&group_id, expected).await;
                }
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token_with_group(node, group_id.clone());
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::ParallelJoin { expected } => {
                let group_id = instance.tokens[token_idx]
                    .parallel_group_id
                    .clone()
                    .unwrap_or_default();
                let done = if let Some(ref join_repo) = ctx.parallel_join_repo {
                    join_repo.try_join(&group_id).await.unwrap_or(false)
                } else {
                    let key = format!("{}:{}:{}", e.instance_id, e.node_id, group_id);
                    let mut state = self.join_state.lock().unwrap();
                    let (exp, arrived) = state
                        .entry(key.clone())
                        .or_insert((*expected, HashSet::new()));
                    arrived.insert(e.token_id.clone());
                    let done = arrived.len() >= *exp;
                    if done {
                        state.remove(&key);
                    }
                    done
                };

                if done {
                    instance.tokens.retain(|t| {
                        !(t.node_id == e.node_id
                            && t.parallel_group_id.as_deref() == Some(group_id.as_str()))
                    });
                    let new_tokens = move_token(node);
                    for t in &new_tokens {
                        out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                            instance_id: e.instance_id.clone(),
                            token_id: t.id.clone(),
                            node_id: t.node_id.clone(),
                        }));
                    }
                    instance.tokens.extend(new_tokens);
                } else {
                    instance.tokens[token_idx].status = TokenStatus::Waiting;
                }
            }
            NodeType::TimerIntermediateCatch { .. } => {
                // Timer events are handled via the timer scheduler;
                // mark the token as waiting until the timer fires.
                instance.tokens[token_idx].status = TokenStatus::Waiting;
            }
            NodeType::BoundaryTimer { .. } | NodeType::BoundaryError { .. } => {
                // Boundary events are not directly activated by token arrival;
                // they are handled by the host activity's event processing.
                instance.tokens[token_idx].status = TokenStatus::Waiting;
            }
            NodeType::CallActivity { called_process_key } => {
                // Look up the called process definition by key
                let active = process_def_store
                    .get_active(called_process_key)
                    .await
                    .ok()
                    .flatten();
                if active.is_none() {
                    warn!(
                        instance_id = %e.instance_id,
                        called_key = %called_process_key,
                        "callActivity: no active process definition found"
                    );
                    instance.tokens[token_idx].status = TokenStatus::Waiting;
                    let _ = process_store.save(&instance).await;
                    return vec![EngineEvent::TokenFailed(payloads::TokenFailed {
                        instance_id: e.instance_id.clone(),
                        token_id: e.token_id.clone(),
                        node_id: e.node_id.clone(),
                        reason: format!(
                            "CallActivity: no active process definition for key '{}'",
                            called_process_key
                        ),
                    })];
                }
                let child_instance_id = uuid::Uuid::new_v4().to_string();
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let _ = process_store.save(&instance).await;
                out.push(EngineEvent::ProcessStarted(payloads::ProcessStarted {
                    process_id: active.unwrap().id.clone(),
                    instance_id: child_instance_id.clone(),
                    initial_variables: Some(instance.variables.clone()),
                }));
                out.push(EngineEvent::CallActivityStarted(
                    payloads::CallActivityStarted {
                        parent_instance_id: e.instance_id.clone(),
                        parent_token_id: e.token_id.clone(),
                        child_instance_id,
                        child_process_key: called_process_key.clone(),
                    },
                ));
            }
            NodeType::MessageIntermediateCatch { .. }
            | NodeType::SignalIntermediateCatch { .. } => {
                // Wait for the message/signal to arrive; token stays in Waiting.
                instance.tokens[token_idx].status = TokenStatus::Waiting;
            }
            NodeType::MessageIntermediateThrow { message_name } => {
                // Send the message and continue.
                out.push(EngineEvent::MessageSent(payloads::MessageSent {
                    instance_id: e.instance_id.clone(),
                    message_name: message_name.clone(),
                }));
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token(node);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::SignalIntermediateThrow { signal_name } => {
                // Fire the signal and continue.
                out.push(EngineEvent::SignalSent(payloads::SignalSent {
                    instance_id: e.instance_id.clone(),
                    signal_name: signal_name.clone(),
                }));
                instance.tokens[token_idx].status = TokenStatus::Waiting;
                let new_tokens = move_token(node);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: e.instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                instance.tokens.extend(new_tokens);
            }
            NodeType::TerminateEnd => {
                // Terminate all active tokens in the instance.
                instance.state = InstanceState::Terminated;
                instance.tokens.clear();
                let _ = process_store.save(&instance).await;
                return vec![EngineEvent::ProcessTerminated(
                    payloads::ProcessTerminated {
                        instance_id: e.instance_id.clone(),
                    },
                )];
            }
        }

        let _ = process_store.save(&instance).await;
        out
    }
}

/// Parse an ISO 8601 duration string (e.g. "PT30S", "PT1H", "PT2M") to a unix epoch timestamp.
///
/// Returns the absolute time (in seconds since epoch) when the timer should fire.
/// Falls back to 60 seconds from now if parsing fails.
fn parse_duration_to_epoch(duration: &str) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let secs = parse_iso8601_duration_secs(duration).unwrap_or(60);
    now + secs
}

/// Parse ISO 8601 duration to seconds. Supports PT{N}S, PT{N}M, PT{N}H.
fn parse_iso8601_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim().strip_prefix('P')?.strip_prefix('T')?;
    let mut total = 0u64;
    let mut num = 0u64;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num = num * 10 + c.to_digit(10)? as u64;
        } else {
            match c {
                'S' => total += num,
                'M' => total += num * 60,
                'H' => total += num * 3600,
                _ => return None,
            }
            num = 0;
        }
    }
    Some(total)
}
