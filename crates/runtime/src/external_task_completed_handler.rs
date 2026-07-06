//! ExternalTaskCompletedHandler: ExternalTaskCompleted -> merge variables, move token, emit TokenArrived.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent};
use tracing::info;

use super::handler::{EngineContext, EventHandler};
use super::transition::move_token;

/// Handler that advances tokens when an external task is completed by a worker.
///
/// This handler mirrors `UserTaskCompletedHandler` but for external tasks:
/// 1. Merges worker-returned variables into the process instance
/// 2. Removes the completed token (by token_id)
/// 3. Creates new tokens via `move_token`
/// 4. Saves the updated instance
/// 5. Emits `TokenArrived` events for each new token
pub struct ExternalTaskCompletedHandler;

#[async_trait]
impl EventHandler for ExternalTaskCompletedHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ExternalTaskCompleted(e) = event else {
            return vec![];
        };
        info!(
            instance_id = %e.instance_id,
            token_id = %e.token_id,
            node_id = %e.node_id,
            "external task completed"
        );
        let process_store = &ctx.process_store;
        let process_def_store = &ctx.process_def_store;
        let Ok(Some(mut instance)) = process_store.load(&e.instance_id).await else {
            return vec![];
        };
        let Ok(Some(def)) = process_def_store.load(&instance.process_def_id).await else {
            return vec![];
        };
        let Some(node) = def.nodes.get(e.node_id.as_str()) else {
            return vec![];
        };
        // Merge worker-returned variables into instance
        for (k, v) in &e.variables {
            instance.variables.insert(k.clone(), v.clone());
        }
        // Remove the completed token by token_id
        instance.tokens.retain(|t| t.id != e.token_id);
        let new_tokens = move_token(node);
        let mut out = vec![];
        for t in &new_tokens {
            out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                instance_id: e.instance_id.clone(),
                token_id: t.id.clone(),
                node_id: t.node_id.clone(),
            }));
        }
        instance.tokens.extend(new_tokens);
        let _ = process_store.save(&instance).await;
        out
    }
}
