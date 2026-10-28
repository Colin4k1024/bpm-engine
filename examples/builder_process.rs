//! Typed process builder quickstart (issue #21 / E4-3).
//!
//! Builds a flow with the Rust builder — no BPMN XML, no DSL JSON strings:
//!
//!   start -> external_task(validate) -> exclusive gateway
//!     | when approved == "true" -> end-approved
//!     | default                  -> end-rejected
//!
//! deploys it to the in-memory stores and runs it to completion through the
//! engine event loop. The "worker" here completes the external task inline
//! with the variable the gateway routes on.
//!
//! Run:
//!   cargo run --example builder_process

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

/// Run an event through the engine with the standard handlers, draining all
/// follow-up events (the same wiring `tests/e2e_smoke.rs` uses).
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Build and compile the definition. `.build()` runs the shared BPMN
    //    compiler, so the flow is validated exactly like XML/DSL input.
    let def = ProcessBuilder::new("builder-demo")
        .process_name("Builder Demo")
        .start_node("start")
        .external_task("validate", "validation")
        .name("Validate order")
        .retries(3)
        .timeout_secs(60)
        .exclusive_gateway("decide")
        .end_node("end-approved")
        .end_node("end-rejected")
        .edge("start", "validate")
        .edge("validate", "decide")
        .edge("decide", "end-approved")
        .when_variable_eq("approved", "true")
        .edge("decide", "end-rejected")
        .is_default()
        .build()?;
    println!(
        "compiled process `{}` ({} nodes, entry `{}`)",
        def.id,
        def.nodes.len(),
        def.start
    );

    // 2. Deploy to the in-memory stores and wire the engine context.
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(def);

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    // 3. Start an instance: the token advances to `validate` and the engine
    //    creates an external task on the `validation` topic.
    let instance_id = "inst-builder-demo";
    run_engine(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: "builder-demo".to_string(),
            instance_id: instance_id.to_string(),
            initial_variables: None,
        }),
        &mut ctx,
    )
    .await;

    let instance = repo.load(instance_id).await?.expect("instance exists");
    println!(
        "instance {} started: {:?}, tokens: {:?}",
        instance_id,
        instance.state,
        instance
            .tokens
            .iter()
            .map(|t| format!("{:?}@{}", t.status, t.node_id))
            .collect::<Vec<_>>()
    );

    // 4. A worker fetches and locks the task (single owner at a time).
    let tasks = repo
        .fetch_and_lock(
            "demo-worker-1",
            &["validation".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await?;
    let task = tasks
        .first()
        .expect("validation task must exist after start");
    println!(
        "worker locked task {} (state {:?})",
        task.task_id, task.state
    );
    assert_eq!(task.state, ExternalTaskState::Locked);

    // 5. The worker completes the task, returning the variable the gateway
    //    routes on: approved = "true" -> end-approved.
    let mut result = HashMap::new();
    result.insert("approved".to_string(), "true".to_string());
    run_engine(
        EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
            instance_id: instance_id.to_string(),
            token_id: task.token_id.clone(),
            node_id: "validate".to_string(),
            variables: result,
        }),
        &mut ctx,
    )
    .await;

    // 6. Outcome: the gateway routed on the worker's variable and the process
    //    completed at the approved end event.
    let instance = repo.load(instance_id).await?.expect("instance exists");
    println!(
        "instance {} finished: {:?} (variables: {:?})",
        instance_id, instance.state, instance.variables
    );
    assert_eq!(instance.state, InstanceState::Completed);
    assert_eq!(instance.variables.get("approved").unwrap(), "true");
    println!("process completed via the typed builder definition.");

    Ok(())
}
