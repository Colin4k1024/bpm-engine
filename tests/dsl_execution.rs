//! End-to-end semantics tests for DSL-compiled process definitions.
//!
//! Compiles DSL documents with `bpm_engine_dsl::compile` and executes them
//! through the real engine handlers with memory stores (same wiring as
//! `tests/e2e_smoke.rs`): external task completion, exclusive gateway
//! routing driven by variables, and a timer intermediate catch event.
//!
//! This proves the DSL entry shares the BPMN entry's execution semantics
//! (ADR-006), not just its compiled model.

use bpm_engine::bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_engine::bpm_engine_core::{payloads, EngineEvent, ExternalTaskState, InstanceState};
use bpm_engine::bpm_engine_dsl;
use bpm_engine::bpm_engine_runtime::{
    EventHandler, ExternalTaskCompletedHandler, HistoryHandler, ProcessCompletedHandler,
    ProcessStartHandler, TimerFiredHandler, TokenArrivedHandler, UserTaskCompletedHandler,
};
use bpm_engine::bpm_engine_storage::{
    ExternalTaskStore, HistoryRepo, ProcessDefinitionStore as Pds, ProcessInstanceStore,
    TimerStore, TokenStore,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Run an event through the engine with all standard handlers (including the
/// timer-fired handler for the timer test below).
async fn run_engine(event: EngineEvent, ctx: &mut bpm_engine::bpm_engine_runtime::EngineContext) {
    let handlers: Vec<Box<dyn EventHandler>> = vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ExternalTaskCompletedHandler),
        Box::new(TimerFiredHandler),
        Box::new(ProcessCompletedHandler),
        Box::new(HistoryHandler),
    ];

    let mut events = vec![event];
    while let Some(ev) = events.pop() {
        for handler in &handlers {
            events.extend(handler.handle(&ev, ctx).await);
        }
    }
}

/// start -> serviceTask(dsl-work) -> exclusiveGateway:
///   condition variableEq(approved, "true") -> task-yes -> end-yes
///   default                              -> task-no  -> end-no
const GATEWAY_FLOW: &str = r#"{
  "schema_version": 1,
  "process": { "id": "dsl-gateway-flow", "name": "DSL Gateway Flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "work", "type": "serviceTask", "task_type": "dsl-work" },
    { "id": "decide", "type": "exclusiveGateway" },
    { "id": "task-yes", "type": "serviceTask", "task_type": "dsl-yes" },
    { "id": "task-no", "type": "serviceTask", "task_type": "dsl-no" },
    { "id": "end-yes", "type": "end" },
    { "id": "end-no", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "work" },
    { "source": "work", "target": "decide" },
    {
      "source": "decide",
      "target": "task-yes",
      "condition": { "kind": "variableEq", "variable": "approved", "value": "true" }
    },
    { "source": "decide", "target": "task-no", "default": true },
    { "source": "task-yes", "target": "end-yes" },
    { "source": "task-no", "target": "end-no" }
  ]
}"#;

/// Run the gateway flow with the given initial variables; returns the repo
/// and definition store once the external `work` task has been completed
/// (i.e. the gateway has routed).
async fn run_gateway_flow_until_routed(
    instance_id: &str,
    approved: &str,
) -> (Arc<MemoryRepo>, Arc<ProcessDefStore>) {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    let def = bpm_engine_dsl::compile(GATEWAY_FLOW).unwrap();
    def_store.register(def);

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .timer_store(repo.clone() as Arc<dyn TimerStore>)
    .build();

    // Start with the routing variable.
    let mut variables = HashMap::new();
    variables.insert("approved".to_string(), approved.to_string());
    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: "dsl-gateway-flow".to_string(),
        instance_id: instance_id.to_string(),
        initial_variables: Some(variables),
    });
    run_engine(event, &mut ctx).await;

    // The token now waits at `work`; the engine created an external task
    // with the DSL task_type.
    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["dsl-work".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(
        tasks.len(),
        1,
        "dsl-work task must be created from the DSL task_type"
    );
    assert_eq!(tasks[0].task_type, "dsl-work");
    assert_eq!(tasks[0].state, ExternalTaskState::Locked);
    assert_eq!(tasks[0].process_instance_id, instance_id);

    // Worker completes the task -> token moves to the gateway, which routes.
    let complete = EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
        instance_id: instance_id.to_string(),
        token_id: tasks[0].token_id.clone(),
        node_id: "work".to_string(),
        variables: HashMap::new(),
    });
    run_engine(complete, &mut ctx).await;

    (repo, def_store)
}

#[tokio::test]
async fn dsl_flow_routes_exclusive_gateway_on_matching_variable() {
    let (repo, def_store) = run_gateway_flow_until_routed("inst-dsl-1", "true").await;

    let instance = repo.load("inst-dsl-1").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(
        instance.tokens.iter().any(|t| t.node_id == "task-yes"),
        "approved=true must route to task-yes, tokens: {:?}",
        instance.tokens
    );
    assert!(!instance.tokens.iter().any(|t| t.node_id == "task-no"));

    // Complete the branch task so the instance finishes.
    let token = instance
        .tokens
        .iter()
        .find(|t| t.node_id == "task-yes")
        .unwrap();
    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();
    run_engine(
        EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
            instance_id: "inst-dsl-1".to_string(),
            token_id: token.id.clone(),
            node_id: "task-yes".to_string(),
            variables: HashMap::new(),
        }),
        &mut ctx,
    )
    .await;

    let instance = repo.load("inst-dsl-1").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Completed);
}

#[tokio::test]
async fn dsl_flow_routes_exclusive_gateway_to_default_branch() {
    let (repo, _def_store) = run_gateway_flow_until_routed("inst-dsl-2", "false").await;

    let instance = repo.load("inst-dsl-2").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(
        instance.tokens.iter().any(|t| t.node_id == "task-no"),
        "approved=false must take the default branch to task-no, tokens: {:?}",
        instance.tokens
    );
    assert!(!instance.tokens.iter().any(|t| t.node_id == "task-yes"));
}

// ---------------------------------------------------------------------------
// variableEq values containing EL boolean keywords (" and " / " or ") must
// still match exactly: the EL splits boolean operators only OUTSIDE quoted
// literals, so the canonical expression `${var == "salt and pepper"}` keeps
// exact-string semantics (regression for the quote-aware EL fix).
// ---------------------------------------------------------------------------

const SPACED_FLOW: &str = r#"{
  "schema_version": 1,
  "process": { "id": "dsl-spaced-flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "work", "type": "serviceTask", "task_type": "spaced-work" },
    { "id": "decide", "type": "exclusiveGateway" },
    { "id": "task-yes", "type": "serviceTask", "task_type": "spaced-yes" },
    { "id": "task-no", "type": "serviceTask", "task_type": "spaced-no" },
    { "id": "end-yes", "type": "end" },
    { "id": "end-no", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "work" },
    { "source": "work", "target": "decide" },
    {
      "source": "decide",
      "target": "task-yes",
      "condition": { "kind": "variableEq", "variable": "flavor", "value": "salt and pepper" }
    },
    { "source": "decide", "target": "task-no", "default": true },
    { "source": "task-yes", "target": "end-yes" },
    { "source": "task-no", "target": "end-no" }
  ]
}"#;

async fn run_spaced_flow(instance_id: &str, flavor: &str) -> Arc<MemoryRepo> {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    let def = bpm_engine_dsl::compile(SPACED_FLOW).unwrap();
    def_store.register(def);

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    let mut variables = HashMap::new();
    variables.insert("flavor".to_string(), flavor.to_string());
    run_engine(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: "dsl-spaced-flow".to_string(),
            instance_id: instance_id.to_string(),
            initial_variables: Some(variables),
        }),
        &mut ctx,
    )
    .await;

    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["spaced-work".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    run_engine(
        EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
            instance_id: instance_id.to_string(),
            token_id: tasks[0].token_id.clone(),
            node_id: "work".to_string(),
            variables: HashMap::new(),
        }),
        &mut ctx,
    )
    .await;
    repo
}

#[tokio::test]
async fn dsl_variable_eq_with_boolean_keyword_matches_exactly() {
    let repo = run_spaced_flow("inst-spaced-1", "salt and pepper").await;
    let instance = repo.load("inst-spaced-1").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(
        instance.tokens.iter().any(|t| t.node_id == "task-yes"),
        "exact match must wait at task-yes, tokens: {:?}",
        instance.tokens
    );
    assert!(!instance.tokens.iter().any(|t| t.node_id == "task-no"));
}

#[tokio::test]
async fn dsl_variable_eq_with_boolean_keyword_mismatch_takes_default() {
    let repo = run_spaced_flow("inst-spaced-2", "plain").await;
    let instance = repo.load("inst-spaced-2").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(
        instance.tokens.iter().any(|t| t.node_id == "task-no"),
        "mismatch must wait at task-no (default branch), tokens: {:?}",
        instance.tokens
    );
    assert!(!instance.tokens.iter().any(|t| t.node_id == "task-yes"));
}

// ---------------------------------------------------------------------------
// Timer intermediate catch (start -> wait PT1S -> end)
// ---------------------------------------------------------------------------

/// The engine's token-arrival handler parks the token at the timer node; the
/// scheduler-side wiring (TimerStore record + `TimerFired` event) mirrors
/// `tests/timer_integration.rs`, which covers the same flow for a
/// hand-built definition. Here the definition comes from the DSL, proving the
/// lowered `timer_definition` string drives the runtime path.
#[tokio::test]
async fn dsl_timer_catch_fires_and_completes_process() {
    const TIMER_FLOW: &str = r#"{
  "schema_version": 1,
  "process": { "id": "dsl-timer-flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "wait", "type": "timerIntermediateCatch", "timer": { "duration": "PT1S" } },
    { "id": "end", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "wait" },
    { "source": "wait", "target": "end" }
  ]
}"#;

    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    let def = bpm_engine_dsl::compile(TIMER_FLOW).unwrap();
    def_store.register(def);

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .timer_store(repo.clone() as Arc<dyn TimerStore>)
    .build();

    run_engine(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: "dsl-timer-flow".to_string(),
            instance_id: "inst-dsl-timer".to_string(),
            initial_variables: None,
        }),
        &mut ctx,
    )
    .await;

    // Token waits at the timer catch node.
    let instance = repo.load("inst-dsl-timer").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    let token = instance
        .tokens
        .iter()
        .find(|t| t.node_id == "wait")
        .expect("token must wait at the timer node");

    // The runtime schedules the timer automatically when the token arrives
    // at the catch node (persistent scheduling, ADR-004) — no manual insert.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let due = repo.list_due("99999999999", 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].node_id, "wait");
    let due_at: u64 = due[0].due_at.parse().unwrap();
    assert!(
        due_at > now && due_at <= now + 2,
        "PT1S must schedule now+1s"
    );
    assert!(
        due[0].token_id == token.id,
        "timer must reference the waiting token"
    );

    // Fire it: the TimerFired handler advances the token to the end node.
    run_engine(
        EngineEvent::TimerFired(payloads::TimerFired {
            timer_id: due[0].id.clone(),
            token_id: due[0].token_id.clone(),
            node_id: "wait".to_string(),
        }),
        &mut ctx,
    )
    .await;

    let instance = repo.load("inst-dsl-timer").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Completed);
}
