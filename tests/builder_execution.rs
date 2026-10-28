//! End-to-end semantics tests for builder-compiled process definitions
//! (issue #21 / E4-3).
//!
//! Builds a flow with the typed Rust builder (`bpm_engine_bpmn::builder`),
//! deploys it to memory stores and executes it through the real engine
//! handlers (same wiring as `tests/dsl_execution.rs` and `tests/e2e_smoke.rs`):
//! the external task is fetched and locked like a worker would, completing it
//! drives the exclusive gateway, and routing is asserted on both branches.
//!
//! This proves the builder entry shares the DSL/BPMN entries' execution
//! semantics (ADR-006), not just their compiled model (see
//! `tests/builder_parity.rs`).

use bpm_engine::bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder;
use bpm_engine::bpm_engine_core::{payloads, EngineEvent, ExternalTaskState, InstanceState};
use bpm_engine::bpm_engine_runtime::{
    EventHandler, ExternalTaskCompletedHandler, HistoryHandler, ProcessCompletedHandler,
    ProcessStartHandler, TokenArrivedHandler, UserTaskCompletedHandler,
};
use bpm_engine::bpm_engine_storage::{
    ExternalTaskStore, HistoryRepo, ProcessDefinitionStore as Pds, ProcessInstanceStore, TokenStore,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Run an event through the engine with all standard handlers.
async fn run_engine(event: EngineEvent, ctx: &mut bpm_engine::bpm_engine_runtime::EngineContext) {
    let handlers: Vec<Box<dyn EventHandler>> = vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(UserTaskCompletedHandler),
        Box::new(ExternalTaskCompletedHandler),
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

/// Build start -> external_task(builder-work) -> exclusiveGateway:
///   when_variable_eq(approved, "true") -> task-yes -> end-yes
///   is_default                          -> task-no  -> end-no
///
/// The branch tasks are external tasks too, so the test drives the instance
/// all the way to a terminal state through worker completions.
fn build_gateway_flow() -> bpm_engine_core::ProcessDefinition {
    ProcessBuilder::new("builder-gateway-flow")
        .process_name("Builder Gateway Flow")
        .start_node("start")
        .external_task("work", "builder-work")
        .exclusive_gateway("decide")
        .external_task("task-yes", "builder-yes")
        .external_task("task-no", "builder-no")
        .end_node("end-yes")
        .end_node("end-no")
        .edge("start", "work")
        .edge("work", "decide")
        .edge("decide", "task-yes")
        .when_variable_eq("approved", "true")
        .edge("decide", "task-no")
        .is_default()
        .edge("task-yes", "end-yes")
        .edge("task-no", "end-no")
        .build()
        .expect("builder gateway flow must compile")
}

/// Start an instance with the given `approved` variable, complete the `work`
/// external task with a simulated worker, and return the routed instance.
async fn run_until_routed(instance_id: &str, approved: &str) -> Arc<MemoryRepo> {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(build_gateway_flow());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    let mut variables = HashMap::new();
    variables.insert("approved".to_string(), approved.to_string());
    run_engine(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: "builder-gateway-flow".to_string(),
            instance_id: instance_id.to_string(),
            initial_variables: Some(variables),
        }),
        &mut ctx,
    )
    .await;

    // The token waits at `work`; the engine created an external task with the
    // builder-declared task_type.
    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["builder-work".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(
        tasks.len(),
        1,
        "builder-work task must be created from the builder task_type"
    );
    assert_eq!(tasks[0].task_type, "builder-work");
    assert_eq!(tasks[0].state, ExternalTaskState::Locked);
    assert_eq!(tasks[0].process_instance_id, instance_id);

    // Worker completes the task -> token moves to the gateway, which routes.
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
async fn builder_flow_routes_exclusive_gateway_on_matching_variable() {
    let repo = run_until_routed("inst-builder-1", "true").await;

    let instance = repo.load("inst-builder-1").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(
        instance.tokens.iter().any(|t| t.node_id == "task-yes"),
        "approved=true must route to task-yes, tokens: {:?}",
        instance.tokens
    );
    assert!(!instance.tokens.iter().any(|t| t.node_id == "task-no"));

    // Complete the branch task so the instance finishes at end-yes.
    let repo2 = repo.clone();
    let token = instance
        .tokens
        .iter()
        .find(|t| t.node_id == "task-yes")
        .unwrap();
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(build_gateway_flow());
    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo2.clone() as Arc<dyn ProcessInstanceStore>,
        repo2.clone() as Arc<dyn TokenStore>,
        def_store as Arc<dyn Pds>,
    )
    .history_repo(repo2.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo2.clone() as Arc<dyn ExternalTaskStore>)
    .build();
    run_engine(
        EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
            instance_id: "inst-builder-1".to_string(),
            token_id: token.id.clone(),
            node_id: "task-yes".to_string(),
            variables: HashMap::new(),
        }),
        &mut ctx,
    )
    .await;

    let instance = repo.load("inst-builder-1").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Completed);
}

#[tokio::test]
async fn builder_flow_routes_exclusive_gateway_to_default_branch() {
    let repo = run_until_routed("inst-builder-2", "false").await;

    let instance = repo.load("inst-builder-2").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(
        instance.tokens.iter().any(|t| t.node_id == "task-no"),
        "approved=false must take the default branch to task-no, tokens: {:?}",
        instance.tokens
    );
    assert!(!instance.tokens.iter().any(|t| t.node_id == "task-yes"));
}

#[tokio::test]
async fn builder_flow_worker_variables_drive_gateway_routing() {
    // The completion event merges worker-returned variables into the instance
    // before the token advances, so a variable returned by the worker itself
    // routes the gateway (same semantics as the DSL/BPMN entries).
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(build_gateway_flow());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    run_engine(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: "builder-gateway-flow".to_string(),
            instance_id: "inst-builder-3".to_string(),
            initial_variables: None,
        }),
        &mut ctx,
    )
    .await;

    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["builder-work".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);

    let mut worker_vars = HashMap::new();
    worker_vars.insert("approved".to_string(), "true".to_string());
    run_engine(
        EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
            instance_id: "inst-builder-3".to_string(),
            token_id: tasks[0].token_id.clone(),
            node_id: "work".to_string(),
            variables: worker_vars,
        }),
        &mut ctx,
    )
    .await;

    let instance = repo.load("inst-builder-3").await.unwrap().unwrap();
    assert!(
        instance.tokens.iter().any(|t| t.node_id == "task-yes"),
        "worker-returned approved=true must route to task-yes, tokens: {:?}",
        instance.tokens
    );
    assert_eq!(instance.variables.get("approved").unwrap(), "true");
}

#[tokio::test]
async fn builder_history_is_recorded() {
    // Builder-built definitions produce the same audit trail: every state
    // transition lands in the history repo.
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(
        ProcessBuilder::new("builder-history")
            .start_node("start")
            .end_node("end")
            .edge("start", "end")
            .build()
            .unwrap(),
    );

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .build();

    run_engine(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: "builder-history".to_string(),
            instance_id: "inst-builder-hist".to_string(),
            initial_variables: None,
        }),
        &mut ctx,
    )
    .await;

    let instance = repo.load("inst-builder-hist").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Completed);
    let history = repo
        .list_by_instance("inst-builder-hist", None, None)
        .await
        .unwrap();
    assert!(!history.is_empty(), "history should not be empty");
}
