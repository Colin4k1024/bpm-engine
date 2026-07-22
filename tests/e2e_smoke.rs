//! E2E smoke tests for the BPM engine.
//!
//! Tests the full workflow: deploy BPMN → start instance → complete tasks → verify completion.
//! Uses the engine API directly for reliable, fast tests.

use bpm_engine::bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
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
            let new_events = handler.handle(&ev, ctx).await;
            events.extend(new_events);
        }
    }
}

// ============================================================================
// Test cases
// ============================================================================

#[tokio::test]
async fn e2e_minimal_process_completes() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    // Deploy minimal BPMN (start → end)
    let bpmn = include_str!("fixtures/minimal.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    // Start instance
    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-1".to_string(),
        initial_variables: None,
    });

    run_engine(event, &mut ctx).await;

    // Instance should be completed immediately (start → end)
    let instance = repo.load("inst-1").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Completed);
}

#[tokio::test]
async fn e2e_start_instance_with_variables() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    let bpmn = include_str!("fixtures/minimal.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    let mut variables = HashMap::new();
    variables.insert("order_id".to_string(), "12345".to_string());
    variables.insert("amount".to_string(), "99.99".to_string());

    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-2".to_string(),
        initial_variables: Some(variables),
    });

    run_engine(event, &mut ctx).await;

    let instance = repo.load("inst-2").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Completed);
    assert_eq!(instance.variables.get("order_id").unwrap(), "12345");
    assert_eq!(instance.variables.get("amount").unwrap(), "99.99");
}

#[tokio::test]
async fn e2e_service_task_flow_with_external_tasks() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    // Deploy service task flow (start → task1 → task2 → task3 → end)
    let bpmn = include_str!("fixtures/service_tasks.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    // Start instance
    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-3".to_string(),
        initial_variables: None,
    });

    run_engine(event, &mut ctx).await;

    // Instance should be running (waiting at task1)
    let instance = repo.load("inst-3").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(instance.tokens.iter().any(|t| t.node_id == "task1"));

    // Create external task for task1
    let token = instance
        .tokens
        .iter()
        .find(|t| t.node_id == "task1")
        .unwrap();
    let task_id = repo
        .create(&token.id, "inst-3", "service", 3, 30, HashMap::new())
        .await
        .unwrap();

    // Fetch and lock
    let tasks = repo
        .fetch_and_lock(
            "worker-1",
            &["service".to_string()],
            10,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, task_id);
    assert_eq!(tasks[0].state, ExternalTaskState::Locked);

    // Complete external task via event
    let complete_event = EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
        instance_id: "inst-3".to_string(),
        token_id: token.id.clone(),
        node_id: "task1".to_string(),
        variables: HashMap::new(),
    });

    run_engine(complete_event, &mut ctx).await;

    // Should advance to task2
    let instance = repo.load("inst-3").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(instance.tokens.iter().any(|t| t.node_id == "task2"));
}

#[tokio::test]
async fn e2e_external_task_fail_and_retry() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    let bpmn = include_str!("fixtures/service_tasks.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    // Start instance
    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-4".to_string(),
        initial_variables: None,
    });

    run_engine(event, &mut ctx).await;

    // Create and lock external task
    let instance = repo.load("inst-4").await.unwrap().unwrap();
    let token = instance
        .tokens
        .iter()
        .find(|t| t.node_id == "task1")
        .unwrap();
    let task_id = repo
        .create(&token.id, "inst-4", "service", 3, 30, HashMap::new())
        .await
        .unwrap();

    repo.fetch_and_lock(
        "worker-1",
        &["service".to_string()],
        10,
        Duration::from_secs(30),
    )
    .await
    .unwrap();

    // Fail the task (should retry)
    repo.fail(
        &task_id,
        "worker-1",
        "timeout".to_string(),
        Some(Duration::from_secs(1)),
    )
    .await
    .unwrap();

    // Task should be ready again
    let task = repo.get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.state, ExternalTaskState::Ready);
    assert_eq!(task.retries, 2); // Decremented
}

#[tokio::test]
async fn e2e_instance_history_recorded() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    let bpmn = include_str!("fixtures/minimal.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-5".to_string(),
        initial_variables: None,
    });

    run_engine(event, &mut ctx).await;

    // History should be recorded
    let history = repo.list_by_instance("inst-5", None, None).await.unwrap();
    assert!(!history.is_empty(), "history should not be empty");
}

#[tokio::test]
async fn e2e_parallel_gateway_creates_multiple_tokens() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    let bpmn = include_str!("fixtures/parallel_gateway.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-6".to_string(),
        initial_variables: None,
    });

    run_engine(event, &mut ctx).await;

    // Should have multiple tokens (parallel branches)
    let instance = repo.load("inst-6").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(
        instance.tokens.len() > 1,
        "parallel gateway should create multiple tokens"
    );
}

#[tokio::test]
async fn e2e_exclusive_gateway_takes_default_branch() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    let bpmn = include_str!("fixtures/exclusive_gateway.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    // No variables → takes default branch (rejected → service task)
    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-7".to_string(),
        initial_variables: None,
    });

    run_engine(event, &mut ctx).await;

    // Should be running at the rejected service task (default branch)
    let instance = repo.load("inst-7").await.unwrap().unwrap();
    assert_eq!(instance.state, InstanceState::Running);
    assert!(instance.tokens.iter().any(|t| t.node_id == "rejected"));
}

#[tokio::test]
async fn e2e_external_task_complete_merges_variables() {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    let bpmn = include_str!("fixtures/service_tasks.bpmn");
    let def = bpm_engine::bpm_engine_bpmn::parse_and_compile(bpmn).unwrap();
    def_store.register(def.clone());

    let mut ctx = bpm_engine::bpm_engine_runtime::EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store.clone() as Arc<dyn Pds>,
    )
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .build();

    // Start with initial variables
    let mut initial_vars = HashMap::new();
    initial_vars.insert("order_id".to_string(), "100".to_string());

    let event = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: def.id.to_string(),
        instance_id: "inst-8".to_string(),
        initial_variables: Some(initial_vars),
    });

    run_engine(event, &mut ctx).await;

    let instance = repo.load("inst-8").await.unwrap().unwrap();
    let token = instance
        .tokens
        .iter()
        .find(|t| t.node_id == "task1")
        .unwrap();

    // Create and lock external task
    repo.create(&token.id, "inst-8", "service", 3, 30, HashMap::new())
        .await
        .unwrap();

    repo.fetch_and_lock(
        "worker-1",
        &["service".to_string()],
        10,
        Duration::from_secs(30),
    )
    .await
    .unwrap();

    // Complete with new variables
    let mut worker_vars = HashMap::new();
    worker_vars.insert("result".to_string(), "approved".to_string());

    let complete_event = EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
        instance_id: "inst-8".to_string(),
        token_id: token.id.clone(),
        node_id: "task1".to_string(),
        variables: worker_vars,
    });

    run_engine(complete_event, &mut ctx).await;

    // Variables should be merged
    let instance = repo.load("inst-8").await.unwrap().unwrap();
    assert_eq!(instance.variables.get("order_id").unwrap(), "100");
    assert_eq!(instance.variables.get("result").unwrap(), "approved");
}
