//! Worker execution context integration tests (issue #16, E3-2).
//!
//! REST-level verification over real HTTP (ephemeral port, memory driver)
//! with REAL `bpm-engine-worker-sdk` workers, covering:
//!
//! - **Cancellation**: a handler polling `ctx.cancelled()` finishes
//!   promptly when the worker is shut down mid-task (`Worker::spawn` +
//!   `WorkerHandle::shutdown`), and its result is still submitted.
//! - **Auto lease renewal**: a handler that runs LONGER than the initial
//!   `lock_duration` with `run_with_renewal` completes successfully (the
//!   lease stays valid); without renewal (control) the late complete is
//!   rejected 4xx — at-least-once redelivery.
//! - **Worker crash**: aborting the worker mid-handle (handler ignores
//!   cancellation → drain timeout aborts it) leaves the lease to expire;
//!   the task is redelivered to a second worker which completes it —
//!   at-least-once at the SDK level, complementing E1-3's store-level
//!   tests.
//! - **Structured failure**: a worker returning
//!   `TaskResult::Fail { category: Some(Permanent) }` takes the task
//!   straight to Failed — NO redelivery, DLQ entry, `TokenFailed` event
//!   (exactly like retry exhaustion); `None` (transient) requeues as
//!   today.
//!
//! Memory driver — runs in CI by default (no Docker).

use bpm_engine_core::ExternalTaskState;
use bpm_engine_core::RetryCategory;
use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, EngineConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::{build_app, state::AppState};
use bpm_engine_worker_sdk::{
    run_with_renewal, EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker,
    WorkerConfig,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Process with one ExternalTask (task_type "ctx-work").
const WORKER_CONTEXT_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="ctx-work-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="ctx-work" name="Context Work">
      <extensionElements>
        <taskDefinition type="ctx-work"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="ctx-work"/>
    <sequenceFlow id="f2" sourceRef="ctx-work" targetRef="end"/>
  </process>
</definitions>"#;

const TASK_TYPE: &str = "ctx-work";

struct TestServer {
    base_url: String,
    state: Arc<AppState>,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

/// Build the app and serve it on an ephemeral port (harness pattern from
/// tests/scheduler_workers.rs).
async fn spawn_server() -> TestServer {
    let config = BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        engine: EngineConfig {
            timer_poll_interval_ms: 200,
            timer_batch_size: 100,
            ..Default::default()
        },
        rate_limit: RateLimitConfig {
            requests_per_minute: 1_000_000,
        },
        ..Default::default()
    };
    let app = build_app(&config).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app.router).await;
    });
    TestServer {
        base_url: format!("http://{}", addr),
        state: app.state,
        serve_task,
        timer_cancel: app.timer_cancel,
    }
}

impl TestServer {
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn deploy(client: &reqwest::Client, base_url: &str) {
    let resp = client
        .post(format!("{}/api/v1/process-definitions/deploy", base_url))
        .header("content-type", "application/xml")
        .body(WORKER_CONTEXT_BPMN.to_string())
        .send()
        .await
        .expect("deploy request");
    assert_eq!(resp.status(), 201, "deploy should return 201");
}

async fn start_instance(client: &reqwest::Client, base_url: &str) -> String {
    let resp = client
        .post(format!("{}/api/v1/process-instances", base_url))
        .json(&serde_json::json!({ "process_def_id": "ctx-work-flow" }))
        .send()
        .await
        .expect("start request");
    assert_eq!(resp.status(), 201, "start should return 201");
    let body: serde_json::Value = resp.json().await.expect("json body");
    body["instance_id"]
        .as_str()
        .expect("instance_id in response")
        .to_string()
}

async fn get_instance(
    client: &reqwest::Client,
    base_url: &str,
    instance_id: &str,
) -> serde_json::Value {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            base_url, instance_id
        ))
        .send()
        .await
        .expect("get instance request");
    assert_eq!(resp.status(), 200, "get instance should return 200");
    resp.json().await.expect("instance json")
}

/// Poll GET instance until `status` equals `expected`, panicking after `timeout`.
async fn wait_for_state(
    client: &reqwest::Client,
    base_url: &str,
    instance_id: &str,
    expected: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let body = get_instance(client, base_url, instance_id).await;
        if body["status"] == expected {
            return body;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "instance {} did not reach {} within {:?}; last state: {}",
                instance_id, expected, timeout, body["status"]
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn history_events(
    client: &reqwest::Client,
    base_url: &str,
    instance_id: &str,
) -> Vec<serde_json::Value> {
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}/history",
            base_url, instance_id
        ))
        .send()
        .await
        .expect("history request");
    assert_eq!(resp.status(), 200, "history should return 200");
    let body: serde_json::Value = resp.json().await.expect("history json");
    body["events"]
        .as_array()
        .cloned()
        .expect("events array in history response")
}

fn count_events(events: &[serde_json::Value], event_type: &str) -> usize {
    events
        .iter()
        .filter(|e| e["event_type"] == event_type)
        .count()
}

/// Worker config for tests: fast poll, short lease, no fetch retries.
fn test_config(worker_id: &str, lock_duration: Duration) -> WorkerConfig {
    WorkerConfig::new(worker_id)
        .poll_interval(Duration::from_millis(50))
        .lock_duration(lock_duration)
        .fetch_retry_max(0)
        .shutdown_grace(Duration::from_secs(5))
}

// ---------------------------------------------------------------------------
// Test 1: cancellation — handler polls ctx.cancelled(), worker shutdown
// ---------------------------------------------------------------------------

/// Handler that "works" for a long time but polls `ctx.cancelled()` and
/// returns as soon as the worker is shutting down; reports the task id and
/// its completion time through channels.
struct CancelAwareHandler {
    started: tokio::sync::mpsc::Sender<String>,
    finished: tokio::sync::mpsc::Sender<Instant>,
}

#[async_trait::async_trait]
impl TaskHandler for CancelAwareHandler {
    fn task_type(&self) -> &str {
        TASK_TYPE
    }

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
        let _ = self.started.send(task.task_id.clone()).await;
        let work = tokio::time::sleep(Duration::from_secs(300));
        tokio::select! {
            _ = ctx.cancelled() => {
                let _ = self.finished.send(Instant::now()).await;
                // A cancelled handler still submits a result: the lease is
                // usually still valid, so the engine records the outcome
                // instead of waiting for the lease to expire.
                TaskResult::Fail {
                    error: "worker_shutdown: handler cancelled".to_string(),
                    retry_after: None,
                    category: None,
                }
            }
            _ = work => TaskResult::Complete {
                variables: HashMap::new(),
            },
        }
    }
}

#[tokio::test]
async fn cancellation_mid_task_finishes_promptly_and_submits_result() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url).await;
    let instance_id = start_instance(&client, &server.base_url).await;

    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<String>(1);
    let (finished_tx, mut finished_rx) = tokio::sync::mpsc::channel::<Instant>(1);
    let worker = Worker::builder()
        .client(EngineClient::new(&server.base_url))
        .handler(CancelAwareHandler {
            started: started_tx,
            finished: finished_tx,
        })
        .config(test_config("w-cancel", Duration::from_secs(30)))
        .build();

    let handle = worker.spawn();
    let task_id = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("worker should fetch the task")
        .expect("handler started");

    // Shut down mid-task. The handler is 300s of "work" away from done —
    // only cancellation can finish it quickly.
    let shutdown_start = Instant::now();
    let drained = handle.shutdown(Duration::from_secs(5)).await;
    assert!(
        drained,
        "handler observes cancellation: drain must succeed within the grace"
    );
    let handler_finished = finished_rx
        .recv()
        .await
        .expect("handler must report its completion");
    let elapsed = handler_finished - shutdown_start;
    assert!(
        elapsed < Duration::from_secs(2),
        "handler must finish promptly after shutdown, took {elapsed:?}"
    );
    assert!(
        shutdown_start.elapsed() < Duration::from_secs(6),
        "shutdown itself must not stall"
    );

    // The drained result was SUBMITTED: the transient fail (no category)
    // requeued the task — retries decremented, state Ready.
    let task = get_task(&server, &task_id).await;
    assert_eq!(task.state, ExternalTaskState::Ready);
    assert_eq!(task.retries, 2, "transient fail decrements retries");
    assert!(
        task.error_message
            .as_deref()
            .unwrap_or_default()
            .contains("worker_shutdown"),
        "the cancelled handler's error was recorded: {:?}",
        task.error_message
    );

    // No redelivery-by-shutdown surprises: the instance is still RUNNING
    // (the task awaits the next worker) and only ONE lock happened.
    let body = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(body["status"], "RUNNING");
    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(count_events(&events, "ExternalTaskLocked"), 1);
    assert_eq!(count_events(&events, "ExternalTaskFailed"), 1);

    server.stop().await;
}

async fn get_task(server: &TestServer, task_id: &str) -> bpm_engine_core::ExternalTask {
    server
        .state
        .storage
        .external_task_store
        .get(task_id)
        .await
        .expect("get task")
        .expect("task row exists")
}

// ---------------------------------------------------------------------------
// Test 2: auto lease renewal — handler outlives the lock and completes
// ---------------------------------------------------------------------------

/// Handler that (a) reports the task id, (b) runs LONGER than the lock
/// duration under `run_with_renewal`, (c) completes.
struct LongHandler {
    started: tokio::sync::mpsc::Sender<String>,
    work: Duration,
}

#[async_trait::async_trait]
impl TaskHandler for LongHandler {
    fn task_type(&self) -> &str {
        TASK_TYPE
    }

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
        let _ = self.started.send(task.task_id.clone()).await;
        // Run under automatic lease renewal: the background task extends
        // the lock every lock_duration/3 while this future runs.
        run_with_renewal(ctx, |_ctx| async move {
            tokio::time::sleep(self.work).await;
            let mut variables = HashMap::new();
            variables.insert("status".to_string(), "DONE".to_string());
            TaskResult::Complete { variables }
        })
        .await
    }
}

#[tokio::test]
async fn auto_renewal_lets_handler_outlive_the_lock() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url).await;
    let instance_id = start_instance(&client, &server.base_url).await;

    // Lock is 1s; the handler "works" 3s (3x the lease) — only renewal
    // keeps the lease valid (renewal interval = 1s/3 ≈ 333ms).
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<String>(1);
    let worker = Worker::builder()
        .client(EngineClient::new(&server.base_url))
        .handler(LongHandler {
            started: started_tx,
            work: Duration::from_secs(3),
        })
        .config(test_config("w-renew", Duration::from_secs(1)))
        .build();

    let handle = worker.spawn();
    let task_id = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("worker should fetch the task")
        .expect("handler started");

    // The complete must succeed (lease still valid thanks to renewal) and
    // the instance must finish.
    wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(15),
    )
    .await;

    let task = get_task(&server, &task_id).await;
    assert_eq!(task.state, ExternalTaskState::Completed);
    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "ExternalTaskCompleted"),
        1,
        "renewal must not cause duplicate completion"
    );
    assert_eq!(count_events(&events, "ProcessCompleted"), 1);

    handle.shutdown(Duration::from_secs(2)).await;
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: control — NO renewal: the late complete is rejected 4xx
// ---------------------------------------------------------------------------

#[tokio::test]
async fn late_complete_without_renewal_is_rejected() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url).await;
    let instance_id = start_instance(&client, &server.base_url).await;

    // Fetch directly (no renewal running) with a 1s lease, then wait past
    // the expiry before completing.
    let engine = EngineClient::new(&server.base_url);
    let tasks = engine
        .fetch_and_lock(engine_worker_id(), &[TASK_TYPE.to_string()], 1, 1_000)
        .await
        .expect("fetch");
    assert_eq!(tasks.len(), 1);
    let task_id = tasks[0].task_id.clone();
    let worker_id = engine_worker_id();

    tokio::time::sleep(Duration::from_millis(1_800)).await;

    let err = engine
        .complete(&task_id, worker_id, HashMap::new())
        .await
        .expect_err("late complete must be rejected");
    match &err {
        bpm_engine_worker_sdk::client::ClientError::Engine { status, message } => {
            assert!(
                *status >= 400 && *status < 500,
                "expected 4xx, got HTTP {status}: {message}"
            );
        }
        other => panic!("expected engine error, got: {other:?}"),
    }

    // At-least-once: the task is redeliverable to the next worker.
    let redelivered = engine
        .fetch_and_lock("w-late-2", &[TASK_TYPE.to_string()], 1, 30_000)
        .await
        .expect("second fetch");
    assert_eq!(redelivered.len(), 1, "expired lease must be redelivered");
    assert_eq!(redelivered[0].task_id, task_id);

    // The instance is still RUNNING (nothing completed it).
    let body = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(body["status"], "RUNNING");

    server.stop().await;
}

fn engine_worker_id() -> &'static str {
    "w-late-1"
}

// ---------------------------------------------------------------------------
// Test 4: worker crash (abort mid-handle) → lease expiry → redelivery
// ---------------------------------------------------------------------------

/// Handler that ignores cancellation and "hangs" — simulating a stuck or
/// crashed worker that will be aborted by the drain timeout.
struct CrashingHandler {
    started: tokio::sync::mpsc::Sender<String>,
}

#[async_trait::async_trait]
impl TaskHandler for CrashingHandler {
    fn task_type(&self) -> &str {
        TASK_TYPE
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let _ = self.started.send(task.task_id.clone()).await;
        // Ignores ctx.cancelled(): only a hard abort (or a crash) ends it.
        tokio::time::sleep(Duration::from_secs(300)).await;
        TaskResult::Complete {
            variables: HashMap::new(),
        }
    }
}

#[tokio::test]
async fn worker_crash_aborts_and_task_is_redelivered_to_second_worker() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url).await;
    let instance_id = start_instance(&client, &server.base_url).await;

    // Worker A with a 1s lease picks up the task and "crashes" (the
    // handler hangs; shutdown's zero grace aborts it mid-handle).
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<String>(1);
    let worker_a = Worker::builder()
        .client(EngineClient::new(&server.base_url))
        .handler(CrashingHandler {
            started: started_tx,
        })
        .config(test_config("w-crash-a", Duration::from_secs(1)))
        .build();
    let handle_a = worker_a.spawn();
    let _task_id = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("worker A should fetch the task")
        .expect("handler started");

    // "Crash": immediate shutdown — the handler ignores cancellation, so
    // the drain aborts it and NO fail/complete is submitted for A.
    let drained = handle_a.shutdown(Duration::ZERO).await;
    assert!(!drained, "hanging handler must be aborted, not drained");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Worker A's task is still Locked (lease not yet expired, owner A).
    // Wait past the 1s lease: the engine reclaims it on the next fetch.
    tokio::time::sleep(Duration::from_millis(1_200)).await;

    // Worker B (a normal, completing worker) receives the SAME task.
    let (b_started_tx, mut b_started_rx) = tokio::sync::mpsc::channel::<String>(1);
    let worker_b = Worker::builder()
        .client(EngineClient::new(&server.base_url))
        .handler(CompletingHandler {
            started: b_started_tx,
        })
        .config(test_config("w-crash-b", Duration::from_secs(30)))
        .build();
    let handle_b = worker_b.spawn();
    let b_task_id = tokio::time::timeout(Duration::from_secs(10), b_started_rx.recv())
        .await
        .expect("worker B should receive the redelivered task")
        .expect("handler B started");

    // Worker B completes; the instance finishes.
    wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(15),
    )
    .await;

    // At-least-once at the SDK level: locked twice (A, then B after the
    // lease expiry), completed exactly once.
    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "ExternalTaskLocked"),
        2,
        "task must be locked twice (initial + redelivery after crash)"
    );
    assert_eq!(count_events(&events, "ExternalTaskCompleted"), 1);
    assert_eq!(count_events(&events, "ProcessCompleted"), 1);
    let task = get_task(&server, &b_task_id).await;
    assert_eq!(task.state, ExternalTaskState::Completed);

    handle_b.shutdown(Duration::from_secs(2)).await;
    server.stop().await;
}

/// Plain handler that completes immediately; reports the task id.
struct CompletingHandler {
    started: tokio::sync::mpsc::Sender<String>,
}

#[async_trait::async_trait]
impl TaskHandler for CompletingHandler {
    fn task_type(&self) -> &str {
        TASK_TYPE
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let _ = self.started.send(task.task_id.clone()).await;
        let mut variables = HashMap::new();
        variables.insert("status".to_string(), "DONE".to_string());
        TaskResult::Complete { variables }
    }
}

// ---------------------------------------------------------------------------
// Test 5: structured failure — permanent vs transient
// ---------------------------------------------------------------------------

/// Handler that fails permanently via the typed TaskResult category.
struct PermanentFailHandler {
    started: tokio::sync::mpsc::Sender<String>,
}

#[async_trait::async_trait]
impl TaskHandler for PermanentFailHandler {
    fn task_type(&self) -> &str {
        TASK_TYPE
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let _ = self.started.send(task.task_id.clone()).await;
        TaskResult::Fail {
            error: "bad_input: amount is not a number".to_string(),
            retry_after: None,
            category: Some(RetryCategory::Permanent),
        }
    }
}

#[tokio::test]
async fn permanent_failure_skips_retries_dlq_and_token_failed() {
    let server = spawn_server().await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url).await;
    let instance_id = start_instance(&client, &server.base_url).await;

    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<String>(1);
    let worker = Worker::builder()
        .client(EngineClient::new(&server.base_url))
        .handler(PermanentFailHandler {
            started: started_tx,
        })
        .config(test_config("w-perm", Duration::from_secs(30)))
        .build();
    let handle = worker.spawn();
    let task_id = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("worker should fetch the task")
        .expect("handler started");

    // Wait for the fail to be submitted (drain waits for the wrapper).
    let drained = handle.shutdown(Duration::from_secs(5)).await;
    assert!(drained);

    // Task: straight to Failed, retries NOT decremented (3 = BPMN default).
    let task = get_task(&server, &task_id).await;
    assert_eq!(task.state, ExternalTaskState::Failed);
    assert_eq!(task.retries, 3, "permanent fail must not consume retries");
    assert!(
        task.error_message
            .as_deref()
            .unwrap_or_default()
            .starts_with(r#"{"retry_category":"permanent"}: bad_input"#),
        "structured category prefix expected: {:?}",
        task.error_message
    );

    // NO redelivery: fetch returns empty.
    let engine = EngineClient::new(&server.base_url);
    let refetched = engine
        .fetch_and_lock("w-perm-2", &[TASK_TYPE.to_string()], 1, 30_000)
        .await
        .expect("fetch after permanent failure");
    assert!(
        refetched.is_empty(),
        "a permanently failed task must not be redelivered"
    );

    // DLQ entry created (the same path retry exhaustion takes).
    let dlq = server
        .state
        .storage
        .dead_letter_store
        .list(None, 100)
        .await
        .expect("dead letter list");
    let entry = dlq
        .iter()
        .find(|e| e.task_id == task_id)
        .expect("dead letter entry for the permanently failed task");
    assert!(
        entry
            .error_message
            .starts_with(r#"{"retry_category":"permanent"}: bad_input"#),
        "DLQ error carries the structured prefix: {}",
        entry.error_message
    );

    // TokenFailed fired (the retry-exhaustion event), exactly once; the
    // ExternalTaskFailed history carries the structured error.
    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(count_events(&events, "ExternalTaskFailed"), 1);
    assert_eq!(count_events(&events, "TokenFailed"), 1);
    assert_eq!(count_events(&events, "ExternalTaskCompleted"), 0);

    // Instance semantics are exactly retry exhaustion's: still RUNNING,
    // the token stays at the external task node (recoverable via DLQ
    // requeue), never silently completed.
    let body = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(body["status"], "RUNNING");
    let nodes: Vec<String> = body["current_nodes"]
        .as_array()
        .expect("current_nodes")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        nodes.contains(&"ctx-work".to_string()),
        "token stays at the task node after permanent failure: {nodes:?}"
    );

    server.stop().await;
}
