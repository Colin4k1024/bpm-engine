//! HistoryHandler: append every EngineEvent to HistoryRepo for Trace UI.

use async_trait::async_trait;
use bpm_engine_core::{redact_history_variable_value, EngineEvent, HistoryRedactionPolicy};
use std::time::{SystemTime, UNIX_EPOCH};

use super::handler::{EngineContext, EventHandler};

/// Payload keys known to carry variable maps (`HashMap<String, String>`).
/// Only these fields are rewritten by history redaction — everything else in
/// the payload is appended verbatim.
const VARIABLE_PAYLOAD_KEYS: [&str; 2] = ["variables", "initial_variables"];

/// Redact variable values in a serialized event payload (E2-3).
///
/// Walks the payload object and rewrites **only** the known variable-carrying
/// fields (`variables`, `initial_variables`) wherever they appear: every
/// string entry is passed through [`redact_history_variable_value`]
/// (oversized → `__truncated:` marker, unknown envelope version →
/// `__opaque:`; everything else verbatim). No other payload field is touched.
fn redact_payload_variables(payload: &mut serde_json::Value, policy: &HistoryRedactionPolicy) {
    let serde_json::Value::Object(map) = payload else {
        return;
    };
    for (key, value) in map {
        if VARIABLE_PAYLOAD_KEYS.contains(&key.as_str()) {
            if let serde_json::Value::Object(variables) = value {
                for variable_value in variables.values_mut() {
                    if let serde_json::Value::String(s) = variable_value {
                        *s = redact_history_variable_value(s, policy);
                    }
                }
            }
        } else {
            redact_payload_variables(value, policy);
        }
    }
}

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
        EngineEvent::CallActivityStarted(p) => Some((
            "CallActivityStarted",
            p.parent_instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::CallActivityCompleted(p) => Some((
            "CallActivityCompleted",
            p.parent_instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::MessageSent(p) => Some((
            "MessageSent",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::SignalSent(p) => Some((
            "SignalSent",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::ProcessTerminated(p) => Some((
            "ProcessTerminated",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
        EngineEvent::ExternalTaskCompleted(p) => Some((
            "ExternalTaskCompleted",
            p.instance_id.clone(),
            serde_json::to_value(p).unwrap_or_default(),
        )),
    }
}

/// Handler that records every engine event to the history repository.
pub struct HistoryHandler;

#[async_trait]
impl EventHandler for HistoryHandler {
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent> {
        let Some((event_type, instance_id, mut payload)) = event_type_instance_payload(event)
        else {
            return vec![];
        };
        if instance_id.is_empty() && event_type != "TimerFired" {
            return vec![];
        }
        let Some(history_repo) = ctx.history_repo.as_ref() else {
            return vec![];
        };
        // Big text must not land in event history: redact variable values
        // (oversized → __truncated:, unknown envelope version → __opaque:)
        // before appending. Redaction applies to new writes only — existing
        // history rows are never rewritten.
        redact_payload_variables(&mut payload, &ctx.history_redaction_policy);
        let occurred_at = occurred_at_now();
        let _ = history_repo
            .append(&instance_id, event_type, &payload, &occurred_at)
            .await;
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_adapter_memory::MemoryRepo;
    use bpm_engine_core::payloads;
    use bpm_engine_storage::HistoryRepo;
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn test_ctx() -> (EngineContext, Arc<MemoryRepo>) {
        let repo = Arc::new(MemoryRepo::new());
        let def_store = Arc::new(bpm_engine_adapter_memory::ProcessDefStore::new());
        let ctx = EngineContext::builder(
            repo.clone() as Arc<dyn bpm_engine_storage::ProcessInstanceStore>,
            repo.clone() as Arc<dyn bpm_engine_storage::TokenStore>,
            def_store as Arc<dyn bpm_engine_storage::ProcessDefinitionStore>,
        )
        .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
        .build();
        (ctx, repo)
    }

    async fn append_and_load(event: EngineEvent, ctx: &mut EngineContext) -> serde_json::Value {
        HistoryHandler.handle(&event, ctx).await;
        let repo = ctx.history_repo.as_ref().unwrap();
        let events = repo.list_by_instance("inst-1", None, None).await.unwrap();
        assert_eq!(events.len(), 1);
        events[0].payload.clone()
    }

    fn started_event(variables: HashMap<String, String>) -> EngineEvent {
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: "p".to_string(),
            instance_id: "inst-1".to_string(),
            initial_variables: Some(variables),
        })
    }

    fn completed_event(variables: HashMap<String, String>) -> EngineEvent {
        EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
            task_id: "t".to_string(),
            instance_id: "inst-1".to_string(),
            node_id: "n".to_string(),
            variables,
        })
    }

    #[tokio::test]
    async fn history_truncates_oversized_variable() {
        let (mut ctx, _repo) = test_ctx().await;
        let mut vars = HashMap::new();
        vars.insert("big".to_string(), "x".repeat(2048));
        let payload = append_and_load(started_event(vars), &mut ctx).await;
        let redacted = payload["initial_variables"]["big"].as_str().unwrap();
        assert!(
            redacted.starts_with("__truncated:2048:"),
            "expected __truncated: marker, got {redacted}"
        );
        assert!(!redacted.contains("xxxx"));
    }

    #[tokio::test]
    async fn history_keeps_small_variable_verbatim() {
        let (mut ctx, _repo) = test_ctx().await;
        let mut vars = HashMap::new();
        vars.insert("small".to_string(), "hello".to_string());
        // Exactly at the default 1024-byte cap: kept verbatim.
        vars.insert("at_limit".to_string(), "y".repeat(1024));
        let payload = append_and_load(started_event(vars), &mut ctx).await;
        assert_eq!(payload["initial_variables"]["small"], "hello");
        assert_eq!(payload["initial_variables"]["at_limit"], "y".repeat(1024));
    }

    #[tokio::test]
    async fn history_encrypted_value_passes_through_as_ciphertext() {
        let (mut ctx, _repo) = test_ctx().await;
        let mut vars = HashMap::new();
        vars.insert(
            "secret".to_string(),
            "__encrypted:QUFBRA==:QkJCQg==".to_string(),
        );
        let payload = append_and_load(started_event(vars), &mut ctx).await;
        assert_eq!(
            payload["initial_variables"]["secret"],
            "__encrypted:QUFBRA==:QkJCQg=="
        );
    }

    #[tokio::test]
    async fn history_unknown_envelope_version_is_opaque() {
        let (mut ctx, _repo) = test_ctx().await;
        let mut vars = HashMap::new();
        vars.insert(
            "future".to_string(),
            "__var:2:{\"t\":\"json\",\"val\":1}".to_string(),
        );
        let payload = append_and_load(started_event(vars), &mut ctx).await;
        assert_eq!(payload["initial_variables"]["future"], "__opaque:");
    }

    #[tokio::test]
    async fn history_redacts_user_task_completed_variables() {
        let (mut ctx, _repo) = test_ctx().await;
        let mut vars = HashMap::new();
        vars.insert("result".to_string(), "z".repeat(5000));
        let payload = append_and_load(completed_event(vars), &mut ctx).await;
        assert!(
            payload["variables"]["result"]
                .as_str()
                .unwrap()
                .starts_with("__truncated:5000:"),
            "UserTaskCompleted.variables must be redacted too"
        );
    }

    #[tokio::test]
    async fn history_non_variable_fields_untouched() {
        let (mut ctx, _repo) = test_ctx().await;
        let vars = HashMap::new();
        let mut event = completed_event(vars);
        if let EngineEvent::UserTaskCompleted(p) = &mut event {
            p.task_id = "t".repeat(2000);
        }
        let payload = append_and_load(event, &mut ctx).await;
        // task_id is not a variable-carrying field: appended verbatim even
        // though it exceeds the redaction cap.
        assert_eq!(payload["task_id"].as_str().unwrap().len(), 2000);
    }

    #[tokio::test]
    async fn history_respects_custom_policy() {
        let repo = Arc::new(MemoryRepo::new());
        let def_store = Arc::new(bpm_engine_adapter_memory::ProcessDefStore::new());
        let mut ctx = EngineContext::builder(
            repo.clone() as Arc<dyn bpm_engine_storage::ProcessInstanceStore>,
            repo.clone() as Arc<dyn bpm_engine_storage::TokenStore>,
            def_store as Arc<dyn bpm_engine_storage::ProcessDefinitionStore>,
        )
        .history_repo(repo as Arc<dyn HistoryRepo>)
        .history_redaction_policy(HistoryRedactionPolicy {
            max_variable_bytes: 4,
        })
        .build();
        let mut vars = HashMap::new();
        vars.insert("v".to_string(), "hello".to_string());
        let payload = append_and_load(started_event(vars), &mut ctx).await;
        assert_eq!(
            payload["initial_variables"]["v"],
            "__truncated:5:2cf24dba5fb0:"
        );
    }

    #[test]
    fn redact_payload_variables_only_touches_known_keys() {
        let mut payload = serde_json::json!({
            "instance_id": "i-1",
            "nested": { "variables": { "a": "x".repeat(3000) } },
            "other": { "map": { "b": "y".repeat(3000) } }
        });
        let policy = HistoryRedactionPolicy::default();
        redact_payload_variables(&mut payload, &policy);
        assert!(payload["nested"]["variables"]["a"]
            .as_str()
            .unwrap()
            .starts_with("__truncated:3000:"));
        // "other.map" is not a variable-carrying key: verbatim.
        assert_eq!(payload["other"]["map"]["b"].as_str().unwrap().len(), 3000);
        assert_eq!(payload["instance_id"], "i-1");
    }
}
