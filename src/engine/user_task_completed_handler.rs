//! UserTaskCompletedHandler: UserTaskCompleted -> move token, emit TokenArrived (design: handler.md §8.3).

use crate::engine::events::{payloads, EngineEvent};
use crate::engine::handler::{EngineContext, EventHandler};
use crate::engine::transition::move_token;
use tracing::info;

pub struct UserTaskCompletedHandler;

impl EventHandler for UserTaskCompletedHandler {
    fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
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
        let Some(mut instance) = process_repo.load(&e.instance_id) else {
            return vec![];
        };
        let Some(def) = process_def_repo.load(&instance.process_def_id) else {
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
        process_repo.save(&instance);
        out
    }
}
