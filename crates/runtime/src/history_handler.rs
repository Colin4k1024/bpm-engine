//! HistoryHandler: append every EngineEvent to HistoryRepo for Trace UI.

use async_trait::async_trait;
use bpm_engine_core::EngineEvent;
use std::time::{SystemTime, UNIX_EPOCH};

use super::handler::{EngineContext, EventHandler};

fn occurred_at_now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string()
}

fn event_type_instance_payload(
    event: &EngineEvent,
) -> Option<(&'static str, String, serde_json::Value)> {
    match event {
        EngineEvent::ProcessStarted(p) => Some((
            "ProcessStarted",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::TokenArrived(p) => Some((
            "TokenArrived",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::TokenCompleted(p) => Some((
            "TokenCompleted",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::UserTaskCreated(p) => Some((
            "UserTaskCreated",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::UserTaskCompleted(p) => Some((
            "UserTaskCompleted",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::TimerFired(p) => {
            // TimerFired payload has only timer_id and token_id; store with empty instance_id.
            Some((
                "TimerFired",
                String::new(),
                serde_json::to_value(p).unwrap_or_default(),
            ))
        }
        EngineEvent::TimerScheduled(p) => Some((
            "TimerScheduled",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::TokenFailed(p) => Some((
            "TokenFailed",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::SagaStarted(p) => Some((
            "SagaStarted",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::SagaCompleted(p) => Some((
            "SagaCompleted",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::ProcessCompleted(p) => Some((
            "ProcessCompleted",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
    }
}

pub struct HistoryHandler;

#[async_trait]
impl EventHandler for HistoryHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let Some((event_type, instance_id, payload)) = event_type_instance_payload(event) else {
            return vec![];
        };
        if instance_id.is_empty() && event_type != "TimerFired" {
            return vec![];
        }
        let Some(history_repo) = ctx.history_repo.as_ref() else {
            return vec![];
        };
        let occurred_at = occurred_at_now();
        let _ = history_repo
            .append(&instance_id, event_type, &payload, &occurred_at)
            .await;
        vec![]
    }
}
