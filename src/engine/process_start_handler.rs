//! ProcessStartHandler: ProcessStarted -> create instance + initial token, emit TokenArrived (design: handler.md §8.1).

use crate::engine::events::{payloads, EngineEvent};
use crate::engine::handler::{EngineContext, EventHandler};
use crate::model::{InstanceState, ProcessInstance, Token, TokenMode, TokenStatus};
use tracing::info;

pub struct ProcessStartHandler;

impl EventHandler for ProcessStartHandler {
    fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::ProcessStarted(e) = event else {
            return vec![];
        };
        info!(instance_id = %e.instance_id, process_id = %e.process_id, "process started");
        #[cfg(feature = "observability")]
        metrics::counter!("process_started_total", 1);
        let Some(process_repo) = ctx.process_repo.as_ref() else {
            return vec![];
        };
        let Some(process_def_repo) = ctx.process_def_repo.as_ref() else {
            return vec![];
        };
        let Some(def) = process_def_repo.load(&e.process_id) else {
            return vec![];
        };
        let token_id = uuid::Uuid::new_v4().to_string();
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
            variables: Default::default(),
            state: InstanceState::Running,
            version: 0,
        };
        process_repo.save(&instance);
        vec![EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id: e.instance_id.clone(),
            token_id,
            node_id: def.start.to_string(),
        })]
    }
}
