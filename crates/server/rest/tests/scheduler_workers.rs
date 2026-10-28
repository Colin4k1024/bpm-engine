//! REST-level scheduler & multi-worker lease verification (issue E1-3).
//!
//! Single service, two workers: verifies over real HTTP (ephemeral port,
//! real timer scheduler + timer event drain task from `build_app`) that
//!
//! - `fetch_and_lock` delivers one task to exactly ONE of two concurrent
//!   workers (ADR-001 single-owner under contention);
//! - a duplicate `POST /external-tasks/:id/complete` is rejected with 4xx and
//!   does NOT double-advance the instance (exactly one
//!   `ExternalTaskCompleted` transition);
//! - an expired lease is reclaimed and the task is redelivered to another
//!   worker (at-least-once semantics), while the original owner's late
//!   complete is rejected with 4xx;
//! - a `PT1S` timer intermediate catch event fires end-to-end through the
//!   server's timer scheduler + drain wiring;
//! - a duplicate `TimerFired` engine event does not double-advance the
//!   instance (token final-state-once invariant).
//!
//! Memory driver — runs in CI by default (no Docker).

use bpm_engine_core::{payloads, EngineEvent};
use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, EngineConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::routes::build_ctx_for_timer;
use bpm_engine_server_rest::{build_app, state::AppState};
use bpm_engine_worker_sdk::client::ClientError;
use bpm_engine_worker_sdk::EngineClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Process with one ExternalTask (task_type "lease-work"); completes after
/// the worker completes the task.
const EXTERNAL_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="lease-work-flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="lease-work" name="Lease Work">
      <extensionElements>
        <taskDefinition type="lease-work"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="lease-work"/>
    <sequenceFlow id="f2" sourceRef="lease-work" targetRef="end"/>
  </process>
</definitions>"#;

/// Process with a PT1S timer intermediate catch event (fixture pattern from
/// tests/fixtures/timer_events.bpmn).
const TIMER_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="timer-catch-flow" isExecutable="true">
    <startEvent id="start"/>
    <intermediateCatchEvent id="wait-1s" name="Wait 1 Second">
      <timerEventDefinition>
        <timeDuration>PT1S</timeDuration>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="wait-1s"/>
    <sequenceFlow id="f2" sourceRef="wait-1s" targetRef="end"/>
  </process>
</definitions>"#;

const LEASE_TASK_TYPE: &str = "lease-work";

struct TestServer {
    base_url: String,
    state: Arc<AppState>,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

/// Build the app and serve it on an ephemeral port with a fast timer poll
/// (200 ms) so timer tests are quick but still exercise the real scheduler.
async fn spawn_server(config: &BpmConfig) -> TestServer {
    let app = build_app(config).await.expect("build_app failed");
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
    /// Stop the server: cancel background tasks, abort the serve task.
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn memory_config() -> BpmConfig {
    BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        // Fast poll so timers fire quickly in tests.
        engine: EngineConfig {
            timer_poll_interval_ms: 200,
            timer_batch_size: 100,
            ..Default::default()
        },
        // Poll loops make many requests; keep the rate limiter out of the way.
        rate_limit: RateLimitConfig {
            requests_per_minute: 1_000_000,
        },
        ..Default::default()
    }
}

async fn deploy(client: &reqwest::Client, base_url: &str, bpmn: &str) {
    let resp = client
        .post(format!("{}/api/v1/process-definitions/deploy", base_url))
        .header("content-type", "application/xml")
        .body(bpmn.to_string())
        .send()
        .await
        .expect("deploy request");
    assert_eq!(resp.status(), 201, "deploy should return 201");
}

async fn start_instance(client: &reqwest::Client, base_url: &str, def_id: &str) -> String {
    let resp = client
        .post(format!("{}/api/v1/process-instances", base_url))
        .json(&serde_json::json!({ "process_def_id": def_id }))
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

/// Fetch the instance history events (GET /process-instances/:id/history).
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

/// Count history events of `event_type` (optionally with payload node_id).
fn count_events(events: &[serde_json::Value], event_type: &str, node_id: Option<&str>) -> usize {
    events
        .iter()
        .filter(|e| e["event_type"] == event_type)
        .filter(|e| match node_id {
            Some(n) => e["payload"]["node_id"] == n,
            None => true,
        })
        .count()
}

fn task_types() -> Vec<String> {
    vec![LEASE_TASK_TYPE.to_string()]
}

/// Fetch-and-lock helper returning tasks (panics on transport errors).
async fn fetch(
    worker: &EngineClient,
    worker_id: &str,
    lock_ms: u64,
) -> Result<Vec<bpm_engine_worker_sdk::ExternalTask>, ClientError> {
    worker
        .fetch_and_lock(worker_id, &task_types(), 1, lock_ms)
        .await
}

fn assert_client_error_4xx(err: &ClientError) -> u16 {
    match err {
        ClientError::Engine { status, message } => {
            assert!(
                *status >= 400 && *status < 500,
                "expected a 4xx rejection, got HTTP {}: {}",
                status,
                message
            );
            *status
        }
        other => panic!("expected engine error, got transport error: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 1: two workers fetch concurrently — exactly one delivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dual_worker_single_delivery() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, EXTERNAL_TASK_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "lease-work-flow").await;

    // The instance waits at the external task node.
    let body = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(body["status"], "RUNNING");
    let nodes: Vec<String> = body["current_nodes"]
        .as_array()
        .expect("current_nodes")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        nodes.contains(&"lease-work".to_string()),
        "instance should wait at 'lease-work', got {:?}",
        nodes
    );

    // Worker A and Worker B fetch-and-lock CONCURRENTLY — exactly one wins
    // (single-owner invariant, ADR-001).
    let worker_a = EngineClient::new(&server.base_url);
    let worker_b = EngineClient::new(&server.base_url);
    let (res_a, res_b) = tokio::join!(
        fetch(&worker_a, "worker-a", 30_000),
        fetch(&worker_b, "worker-b", 30_000)
    );
    let tasks_a = res_a.expect("worker-a fetch should succeed");
    let tasks_b = res_b.expect("worker-b fetch should succeed");
    assert_eq!(
        tasks_a.len() + tasks_b.len(),
        1,
        "exactly one worker must receive the task (A got {}, B got {})",
        tasks_a.len(),
        tasks_b.len()
    );

    let (winner_client, winner_worker, winner_task_id) = if tasks_a.len() == 1 {
        (worker_a.clone(), "worker-a", tasks_a[0].task_id.clone())
    } else {
        (worker_b.clone(), "worker-b", tasks_b[0].task_id.clone())
    };

    // The winner completes; the instance finishes.
    winner_client
        .complete(&winner_task_id, winner_worker, HashMap::new())
        .await
        .expect("winner complete should succeed");
    wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(15),
    )
    .await;

    // The loser's late fetch finds nothing (task completed, not re-delivered).
    let loser = if tasks_a.len() == 1 {
        fetch(&worker_b, "worker-b", 30_000).await
    } else {
        fetch(&worker_a, "worker-a", 30_000).await
    }
    .expect("loser fetch should succeed");
    assert!(
        loser.is_empty(),
        "loser must not receive the completed task"
    );

    // History: exactly one ExternalTaskLocked (single delivery at REST level)
    // and exactly one ExternalTaskCompleted / ProcessCompleted.
    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "ExternalTaskLocked", None),
        1,
        "only one worker may lock the task"
    );
    assert_eq!(
        count_events(&events, "ExternalTaskCompleted", None),
        1,
        "exactly one completion transition"
    );
    assert_eq!(count_events(&events, "ProcessCompleted", None), 1);

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: duplicate completion is rejected (4xx), no double advance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn duplicate_completion_is_idempotent() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, EXTERNAL_TASK_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "lease-work-flow").await;

    let worker_a = EngineClient::new(&server.base_url);
    let tasks = fetch(&worker_a, "worker-a", 30_000)
        .await
        .expect("worker-a fetch");
    assert_eq!(tasks.len(), 1);
    let task_id = tasks[0].task_id.clone();

    // First complete succeeds.
    worker_a
        .complete(&task_id, "worker-a", HashMap::new())
        .await
        .expect("first complete should succeed");
    wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(15),
    )
    .await;

    // Snapshot the completed instance (state + tokens; note the engine
    // deliberately keeps the origin/start token in a Waiting status —
    // pre-existing semantics, same as every other flow).
    let after_first = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(after_first["status"], "COMPLETED");

    // The SAME worker posts the SAME complete again: the store-level guard
    // (task already Completed) rejects it with 4xx — no 5xx, no double event.
    let err = worker_a
        .complete(&task_id, "worker-a", HashMap::new())
        .await
        .expect_err("duplicate complete must be rejected");
    let status = assert_client_error_4xx(&err);
    assert_eq!(status, 400, "duplicate complete maps to HTTP 400");

    // The instance is bit-for-bit unchanged: still Completed with the exact
    // same token set — the rejected request never reached the engine event
    // loop, so no second token advance occurred.
    let after_dup = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(after_dup["status"], "COMPLETED");
    assert_eq!(
        after_dup["tokens"], after_first["tokens"],
        "duplicate complete must not change the token set"
    );

    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "ExternalTaskCompleted", None),
        1,
        "exactly one ExternalTaskCompleted transition expected, got: {:?}",
        events
            .iter()
            .map(|e| e["event_type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
    );
    assert_eq!(count_events(&events, "ProcessCompleted", None), 1);
    assert_eq!(
        count_events(&events, "TokenArrived", Some("end")),
        1,
        "token must reach 'end' exactly once"
    );
    assert_eq!(
        count_events(&events, "TokenFailed", None),
        0,
        "no failure events expected"
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: expired lease is reclaimed, task redelivered, late complete rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_lock_is_reclaimed_and_redelivered() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, EXTERNAL_TASK_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "lease-work-flow").await;

    // Worker A locks with a 1-second lease and then "crashes" (never completes).
    let worker_a = EngineClient::new(&server.base_url);
    let worker_b = EngineClient::new(&server.base_url);
    let tasks_a = fetch(&worker_a, "worker-a", 1_000)
        .await
        .expect("worker-a fetch");
    assert_eq!(tasks_a.len(), 1);
    let task_id = tasks_a[0].task_id.clone();

    // Sleep past the lease expiry (epoch-second granularity).
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Worker B fetches: the expired lock is reclaimed and the SAME task is
    // redelivered (at-least-once).
    let tasks_b = fetch(&worker_b, "worker-b", 30_000)
        .await
        .expect("worker-b fetch");
    assert_eq!(
        tasks_b.len(),
        1,
        "worker B must receive the redelivered task"
    );
    assert_eq!(
        tasks_b[0].task_id, task_id,
        "worker B must receive the SAME task id (redelivery, not a new task)"
    );

    // Worker A's late complete is rejected with 4xx (lease conflict: the task
    // is locked by worker B now).
    let err = worker_a
        .complete(&task_id, "worker-a", HashMap::new())
        .await
        .expect_err("late complete by the previous owner must be rejected");
    assert_client_error_4xx(&err);

    // Worker B completes; the instance finishes.
    worker_b
        .complete(&task_id, "worker-b", HashMap::new())
        .await
        .expect("worker-b complete should succeed");
    wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(15),
    )
    .await;

    // History proves at-least-once redelivery: the task was locked twice
    // (worker A, then worker B after reclaim) and completed exactly once.
    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "ExternalTaskLocked", None),
        2,
        "task must be locked twice (initial + redelivery after reclaim)"
    );
    assert_eq!(
        count_events(&events, "ExternalTaskCompleted", None),
        1,
        "exactly one completion even after redelivery"
    );
    assert_eq!(count_events(&events, "ProcessCompleted", None), 1);

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 4: timer fires end-to-end through the server's scheduler + drain
// ---------------------------------------------------------------------------

#[tokio::test]
async fn timer_fires_through_service() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, TIMER_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "timer-catch-flow").await;

    // The instance waits at the timer catch event.
    let body = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(body["status"], "RUNNING");
    let nodes: Vec<String> = body["current_nodes"]
        .as_array()
        .expect("current_nodes")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        nodes.contains(&"wait-1s".to_string()),
        "instance should wait at the timer catch, got {:?}",
        nodes
    );

    // No worker involvement: the timer scheduler + drain task must fire the
    // PT1S timer and drive the instance to Completed (200 ms poll, 10 s cap).
    wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(20),
    )
    .await;

    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "TokenArrived", Some("end")),
        1,
        "timer must advance the token past the catch exactly once"
    );
    assert_eq!(count_events(&events, "ProcessCompleted", None), 1);

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 5: duplicate TimerFired event does not double-advance the instance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn duplicate_timer_event_is_idempotent() {
    let server = spawn_server(&memory_config()).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, TIMER_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "timer-catch-flow").await;

    // The instance is waiting at the timer catch; find its scheduled timer.
    // `list_due` with a far-future "now" returns every Scheduled timer.
    let timers = server
        .state
        .storage
        .timer_store
        .list_due("9999999999", 100)
        .await
        .expect("list timers");
    let timer = timers
        .iter()
        .find(|t| t.instance_id == instance_id)
        .expect("timer record must be scheduled for the waiting instance")
        .clone();
    assert_eq!(timer.status, "Scheduled");

    // Manually inject a DUPLICATE TimerFired engine event through the exact
    // path the server's drain task uses (build_ctx_for_timer + engine). This
    // simulates a redelivered/replayed timer event (e.g. outbox replay after
    // a crash between mark_fired and event dispatch).
    let fire_event = || {
        EngineEvent::TimerFired(payloads::TimerFired {
            timer_id: timer.id.clone(),
            token_id: timer.token_id.clone(),
            node_id: timer.node_id.clone(),
        })
    };
    {
        let mut ctx = build_ctx_for_timer(&server.state);
        server.state.engine.run_async(fire_event(), &mut ctx).await;
    }

    // The first delivery advances the instance past the catch. Snapshot it.
    let after_first = wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(15),
    )
    .await;

    // The scheduler's own sweep will also deliver the real TimerFired (the
    // manual injection above does not mark_fired, so the timer is still due).
    // Wait until the scheduler has processed it so the "real" duplicate is
    // actually delivered before we assert stability.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let rec = server
            .state
            .storage
            .timer_store
            .get_by_id(&timer.id)
            .await
            .expect("get timer")
            .expect("timer record exists");
        if rec.status == "Fired" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "scheduler did not fire the due timer within 5s (status: {})",
            rec.status
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Now inject the SAME TimerFired event a third time (post-completion
    // duplicate). It must be a no-op: no panic, no state corruption.
    {
        let mut ctx = build_ctx_for_timer(&server.state);
        server.state.engine.run_async(fire_event(), &mut ctx).await;
    }

    // The instance is unchanged: still Completed with the exact same token
    // set (the engine keeps the origin/start token Waiting by design) — the
    // duplicate TimerFired resurrected no token and changed no state.
    let body = get_instance(&client, &server.base_url, &instance_id).await;
    assert_eq!(body["status"], "COMPLETED");
    assert_eq!(
        body["tokens"], after_first["tokens"],
        "duplicate TimerFired must not change the token set"
    );

    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "TokenArrived", Some("end")),
        1,
        "the catch must be passed exactly once despite 3 TimerFired deliveries: {:?}",
        events
            .iter()
            .map(|e| e["event_type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
    );
    assert_eq!(count_events(&events, "ProcessCompleted", None), 1);
    assert_eq!(
        count_events(&events, "TokenFailed", None),
        0,
        "duplicate TimerFired must not produce failures"
    );

    // The timer record itself stays in its final Fired state (exactly-once
    // firing at the store level; mark_fired is idempotent).
    let rec = server
        .state
        .storage
        .timer_store
        .get_by_id(&timer.id)
        .await
        .expect("get timer")
        .expect("timer record exists");
    assert_eq!(rec.status, "Fired");

    server.stop().await;
}
