//! TokenArrivedHandler: TokenArrived -> advance token, emit TokenArrived/ProcessCompleted.
//! Handles ParallelFork / ParallelJoin.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent, InstanceState, NodeType, TokenStatus};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tracing::{debug, warn};

use super::handler::{EngineContext, EventHandler};
use super::transition::{
    evaluate_exclusive_gateway, move_token, move_token_preserving_group, move_token_with_group,
};

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

#[async_trait]
impl EventHandler for TokenArrivedHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TokenArrived(e) = event else {
            return vec![];
        };
        let Some(process_store) = ctx.process_store.as_ref() else {
            return vec![];
        };
        let Some(process_def_store) = ctx.process_def_store.as_ref() else {
            return vec![];
        };
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
        if let Some(tr) = ctx.token_store.as_ref() {
            if !tr
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
            NodeType::UserTask => {
                instance.tokens[token_idx].status = TokenStatus::Waiting;
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
        }

        let _ = process_store.save(&instance).await;
        out
    }
}
