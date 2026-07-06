//! ProcessStartHandler: ProcessStarted -> create instance + initial token, emit TokenArrived.

use async_trait::async_trait;
use bpm_engine_core::{
    payloads, EngineEvent, InstanceState, ProcessInstance, Token, TokenMode, TokenStatus,
};
use tracing::info;

use super::handler::{EngineContext, EventHandler};

/// Handler that creates a process instance and initial token on ProcessStarted.
pub struct ProcessStartHandler;

#[async_trait]
impl EventHandler for ProcessStartHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ProcessStarted(e) = event else {
            return vec![];
        };
        info!(instance_id = %e.instance_id, process_id = %e.process_id, "process started");
        let process_store = &ctx.process_store;
        let process_def_store = &ctx.process_def_store;
        let Some(def) = process_def_store.load(&e.process_id).await.ok().flatten() else {
            return vec![];
        };
        let token_id = uuid::Uuid::new_v4().to_string();
        let variables = e.initial_variables.clone().unwrap_or_default();
        let instance = ProcessInstance {
            id: e.instance_id.clone(),
            process_def_id: e.process_id.clone(),
            tenant_id: ctx.tenant_id.clone(),
            tokens: vec![Token {
                id: token_id.clone(),
                node_id: def.start.to_string(),
                status: TokenStatus::Ready,
                mode: TokenMode::Forward,
                version: 0,
                attempt: 0,
                parallel_group_id: None,
                updated_at: None,
            }],
            variables,
            state: InstanceState::Running,
            version: 0,
            parent_instance_id: None,
            parent_token_id: None,
        };
        let _ = process_store.save(&instance).await;
        vec![EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id: e.instance_id.clone(),
            token_id,
            node_id: def.start.to_string(),
        })]
    }
}
