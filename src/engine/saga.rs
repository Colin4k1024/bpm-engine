//! Saga (compensation) model and coordinator (design: saga.md).
//! SagaCoordinator: on TokenFailed, load compensation records, create compensation tokens, emit TokenArrived.

use crate::engine::events::{payloads, EngineEvent};
use crate::engine::handler::{EngineContext, EventHandler};
use crate::model::{Token, TokenMode, TokenStatus};

/// Compensation record (design: saga.md §5.1). Domain type; persistence uses CompensationRecordRow.
#[derive(Debug)]
pub struct CompensationRecord {
    pub instance_id: String,
    pub node_id: String,
    pub order: u32,
    pub status: CompensationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompensationStatus {
    Pending,
    Completed,
    Failed,
}

/// SagaCoordinator: on TokenFailed, start compensation flow (design: saga.md §7, §8).
/// Loads compensation records, creates tokens with TokenMode::Compensation, emits SagaStarted and TokenArrived.
pub struct SagaCoordinator;

impl EventHandler for SagaCoordinator {
    fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let EngineEvent::TokenFailed(e) = event else {
            return vec![];
        };
        let Some(ref compensation_repo) = ctx.compensation_repo else {
            return vec![];
        };
        let Some(ref process_repo) = ctx.process_repo else {
            return vec![];
        };
        let Some(ref token_repo) = ctx.token_repo else {
            return vec![];
        };

        let records = compensation_repo.list_by_instance(&e.instance_id);
        let pending: Vec<_> = records
            .into_iter()
            .filter(|r| r.status == "Pending")
            .collect();
        if pending.is_empty() {
            return vec![];
        }
        // Reverse order for compensation (design: saga.md — execute in reverse).
        let mut ordered = pending;
        ordered.sort_by(|a, b| b.order.cmp(&a.order));

        let Some(mut instance) = process_repo.load(&e.instance_id) else {
            return vec![];
        };

        let mut out = vec![
            EngineEvent::SagaStarted(payloads::SagaStarted {
                instance_id: e.instance_id.clone(),
                token_id: e.token_id.clone(),
                node_id: e.node_id.clone(),
            }),
        ];

        for rec in &ordered {
            let token_id = uuid::Uuid::new_v4().to_string();
            let node_id = rec.node_id.clone();
            instance.tokens.push(Token {
                id: token_id.clone(),
                node_id: node_id.clone(),
                status: TokenStatus::Ready,
                mode: TokenMode::Compensation,
                version: 0,
                attempt: 0,
                parallel_group_id: None,
                updated_at: None,
            });
            out.push(EngineEvent::TokenArrived(payloads::TokenArrived {
                instance_id: e.instance_id.clone(),
                token_id,
                node_id,
            }));
        }

        process_repo.save(&instance);
        token_repo.save_tokens(&instance.id, &instance.tokens);

        out
    }
}
