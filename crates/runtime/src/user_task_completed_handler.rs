//! UserTaskCompletedHandler: UserTaskCompleted -> move token, emit TokenArrived.

use async_trait::async_trait;
use bpm_core::{payloads, EngineEvent};
use tracing::info;

use super::handler::{EngineContext, EventHandler};
use super::transition::move_token;

pub struct UserTaskCompletedHandler;

#[async_trait]
impl EventHandler for UserTaskCompletedHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::UserTaskCompleted(e) = event else {
            return vec![];
        };
        info!(instance_id = %e.instance_id, node_id = %e.node_id, "user task completed");
        let Some(process_repo) = ctx.process_repo.as_ref() else {
            return vec![];
        };
        let Some(process_def_repo) = ctx.process_def_repo.as_ref() else {
            return vec![];
        };
        let Ok(Some(mut instance)) = process_repo.load(&e.instance_id).await else {
            return vec![];
        };
        let Ok(Some(def)) = process_def_repo.load(&instance.process_def_id).await else {
            return vec![];
        };
        let Some(node) = def.nodes.get(e.node_id.as_str()) else {
            return vec![];
        };
        instance.tokens.retain(|t| t.node_id != e.node_id);
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
        let _ = process_repo.save(&instance).await;
        out
    }
}
