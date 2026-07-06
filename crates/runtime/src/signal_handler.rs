//! SignalHandler: resumes tokens waiting at signal intermediate catch events
//! when a matching SignalSent event is fired.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent, NodeType, TokenStatus};
use tracing::debug;

use super::handler::{EngineContext, EventHandler};
use super::transition::move_token_preserving_group;

/// When a SignalSent event is processed, scans all running instances for tokens
/// waiting at a SignalIntermediateCatch node with a matching signal name,
/// and resumes them.
pub struct SignalCatchHandler;

#[async_trait]
impl EventHandler for SignalCatchHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::SignalSent(e) = event else {
            return vec![];
        };
        let process_store = &ctx.process_store;
        let process_def_store = &ctx.process_def_store;

        let Ok(instance_ids) = process_store.list_running(ctx.tenant_id.as_deref()).await else {
            return vec![];
        };

        let mut out = vec![];

        for iid in instance_ids {
            let Ok(Some(mut instance)) = process_store.load(&iid).await else {
                continue;
            };
            if instance.state != bpm_engine_core::InstanceState::Running {
                continue;
            }
            let Ok(Some(def)) = process_def_store.load(&instance.process_def_id).await else {
                continue;
            };

            let mut resumed_token_ids = Vec::new();
            let mut pending_new_tokens = Vec::new();

            // Collect matches first to avoid borrow conflicts
            let matches: Vec<(usize, String, Option<String>)> = instance
                .tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| t.status == TokenStatus::Waiting)
                .filter_map(|(idx, token)| {
                    let node = def.nodes.get(token.node_id.as_str())?;
                    if let NodeType::SignalIntermediateCatch { signal_name } = &node.node_type {
                        if signal_name == &e.signal_name {
                            return Some((idx, token.id.clone(), token.parallel_group_id.clone()));
                        }
                    }
                    None
                })
                .collect();

            for (idx, token_id, parallel_group_id) in matches {
                debug!(
                    instance_id = %iid,
                    token_id = %token_id,
                    signal = %e.signal_name,
                    "signal catch: resuming token"
                );
                let node = def
                    .nodes
                    .get(instance.tokens[idx].node_id.as_str())
                    .unwrap();
                instance.tokens[idx].status = TokenStatus::Waiting;
                let new_tokens = move_token_preserving_group(node, parallel_group_id);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: iid.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                pending_new_tokens.extend(new_tokens);
                resumed_token_ids.push(token_id);
            }

            instance.tokens.extend(pending_new_tokens);

            if !resumed_token_ids.is_empty() {
                let _ = process_store.save(&instance).await;
            }
        }

        out
    }
}
