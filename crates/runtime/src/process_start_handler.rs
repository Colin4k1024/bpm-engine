//! ProcessStartHandler: ProcessStarted -> create instance + initial token, emit TokenArrived.

use async_trait::async_trait;
use bpm_core::{
    payloads, EngineEvent, InstanceState, ProcessInstance, Token, TokenMode, TokenStatus,
};
use tracing::info;

use super::handler::{EngineContext, EventHandler};

pub struct ProcessStartHandler;

#[async_trait]
impl EventHandler for ProcessStartHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ProcessStarted(e) = event else {
            return vec![];
        };
        info!(instance_id = %e.instance_id, process_id = %e.process_id, "process started");
        let Some(process_repo) = ctx.process_repo.as_ref() else {
            return vec![];
        };
        let Some(process_def_repo) = ctx.process_def_repo.as_ref() else {
            return vec![];
        };
        let Some(def) = process_def_repo.load(&e.process_id).await.ok().flatten() else {
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
        };
        let _ = process_repo.save(&instance).await;
        vec![EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id: e.instance_id.clone(),
            token_id,
            node_id: def.start.to_string(),
        })]
    }
}
