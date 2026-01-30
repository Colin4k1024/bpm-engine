//! TokenArrivedHandler: TokenArrived -> advance token, emit TokenArrived/ProcessCompleted (design: handler.md §8.2).
//! Handles ParallelFork / ParallelJoin (design: core.md §5).

use crate::engine::events::{payloads, EngineEvent};
use crate::engine::handler::{EngineContext, EventHandler};
use crate::engine::transition::{evaluate_exclusive_gateway, move_token, move_token_with_group};
use crate::model::{InstanceState, NodeType, TokenStatus};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tracing::{debug, warn};

/// In-memory JoinState: key = "instance_id:node_id:parallel_group_id", value = (expected, arrived token ids).
pub struct TokenArrivedHandler {
    join_state: Mutex<HashMap<String, (usize, HashSet<String>)>>,
}

impl TokenArrivedHandler {
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

impl EventHandler for TokenArrivedHandler {
    fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TokenArrived(e) = event else {
            return vec![];
        };
        let Some(process_repo) = ctx.process_repo.as_ref() else {
            return vec![];
        };
        let Some(process_def_repo) = ctx.process_def_repo.as_ref() else {
            return vec![];
        };
        let Some(mut instance) = process_repo.load(&e.instance_id) else {
            return vec![];
        };
        let Some(def) = process_def_repo.load(&instance.process_def_id) else {
            return vec![];
        };
        let Some(token_idx) = instance
            .tokens
            .iter()
            .position(|t| t.id == e.token_id)
        else {
            return vec![];
        };
        let token = &instance.tokens[token_idx];
        // Whitepaper §11.4: Claim Ready -> Executing; if CAS fails, abandon (no retry).
        if let Some(tr) = ctx.token_repo.as_ref() {
            if !tr.claim_token(&e.instance_id, &token.id, token.version) {
                warn!(instance_id = %e.instance_id, token_id = %token.id, "token claim failed (CAS)");
                #[cfg(feature = "observability")]
                metrics::counter!("token_claim_failures_total", 1);
                return vec![];
            }
            #[cfg(feature = "observability")]
            metrics::counter!("token_claimed_total", 1);
            instance.tokens[token_idx].status = TokenStatus::Executing;
            instance.tokens[token_idx].version += 1;
        }
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
            NodeType::UserTask => {
                println!("⏸ UserTask at node {}", node.id);
                instance.tokens[token_idx].status = TokenStatus::Waiting;
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
                process_repo.save(&instance);
                return vec![EngineEvent::ProcessCompleted(payloads::ProcessCompleted {
                    instance_id: e.instance_id.clone(),
                })];
            }
            NodeType::ParallelFork => {
                let group_id = uuid::Uuid::new_v4().to_string();
                if let Some(ref join_repo) = ctx.parallel_join_repo {
                    let expected = node.outgoing_edges.len() as u32;
                    let _ = join_repo.ensure_group(&group_id, expected);
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
                    join_repo.try_join(&group_id).unwrap_or(false)
                } else {
                    let key = format!("{}:{}:{}", e.instance_id, e.node_id, group_id);
                    let mut state = self.join_state.lock().unwrap();
                    let (exp, arrived) = state.entry(key.clone()).or_insert((*expected, HashSet::new()));
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
        }

        process_repo.save(&instance);
        out
    }
}
