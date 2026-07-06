//! CallActivity handlers: link parent-child instances and resume parent on child completion.

use async_trait::async_trait;
use bpm_engine_core::{payloads, EngineEvent, TokenStatus};
use tracing::{debug, warn};

use super::handler::{EngineContext, EventHandler};
use super::transition::move_token_preserving_group;

/// Sets parent_instance_id / parent_token_id on the child instance when
/// a CallActivityStarted event is processed.
pub struct CallActivityStartedHandler;

#[async_trait]
impl EventHandler for CallActivityStartedHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::CallActivityStarted(e) = event else {
            return vec![];
        };
        let process_store = &ctx.process_store;
        // Load the child instance (should have been created by ProcessStartHandler)
        let Ok(Some(mut child)) = process_store.load(&e.child_instance_id).await else {
            warn!(
                child_id = %e.child_instance_id,
                "CallActivityStarted: child instance not found"
            );
            return vec![];
        };
        child.parent_instance_id = Some(e.parent_instance_id.clone());
        child.parent_token_id = Some(e.parent_token_id.clone());
        let _ = process_store.save(&child).await;
        debug!(
            parent_id = %e.parent_instance_id,
            child_id = %e.child_instance_id,
            "call activity: parent-child linked"
        );
        vec![]
    }
}

/// When a ProcessCompleted event arrives, checks if the completed instance has a parent.
/// If so, resumes the parent token and copies output variables from the child.
pub struct CallActivityCompletionHandler;

#[async_trait]
impl EventHandler for CallActivityCompletionHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ProcessCompleted(e) = event else {
            return vec![];
        };
        let process_store = &ctx.process_store;
        let process_def_store = &ctx.process_def_store;

        // Load the completed instance
        let Ok(Some(child)) = process_store.load(&e.instance_id).await else {
            return vec![];
        };

        // Check if this instance was started by a CallActivity
        let (parent_id, parent_token_id) = match (&child.parent_instance_id, &child.parent_token_id)
        {
            (Some(pid), Some(tid)) => (pid.clone(), tid.clone()),
            _ => return vec![], // Not a child instance
        };

        // Load parent instance
        let Ok(Some(mut parent)) = process_store.load(&parent_id).await else {
            warn!(
                parent_id = %parent_id,
                child_id = %e.instance_id,
                "CallActivityCompletion: parent instance not found"
            );
            return vec![];
        };

        // Find the parent token that was waiting
        let Some(token_idx) = parent.tokens.iter().position(|t| t.id == parent_token_id) else {
            warn!(
                parent_id = %parent_id,
                token_id = %parent_token_id,
                "CallActivityCompletion: parent token not found"
            );
            return vec![];
        };

        // Copy output variables from child to parent
        for (k, v) in &child.variables {
            parent.variables.insert(k.clone(), v.clone());
        }

        // Resume the parent token: set to Waiting, then move to next node
        parent.tokens[token_idx].status = TokenStatus::Waiting;

        // Load parent process definition to get the node for moving the token
        let parent_def = process_def_store
            .load(&parent.process_def_id)
            .await
            .ok()
            .flatten();

        let mut out = vec![];

        if let Some(def) = parent_def {
            if let Some(node) = def.nodes.get(parent.tokens[token_idx].node_id.as_str()) {
                let parallel_group_id = parent.tokens[token_idx].parallel_group_id.clone();
                let new_tokens = move_token_preserving_group(node, parallel_group_id);
                for t in &new_tokens {
                    out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: parent_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }));
                }
                parent.tokens.extend(new_tokens);
            }
        }

        let _ = process_store.save(&parent).await;
        out
    }
}
