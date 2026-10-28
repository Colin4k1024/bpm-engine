//! PostgreSQL-mode scheduler & worker lease verification (issue E1-3).
//!
//! Requires Docker (testcontainers). Run with:
//!
//! ```text
//! cargo test -p bpm-engine-server-rest --test scheduler_workers_postgres -- --ignored
//! ```
//!
//! Same scenarios as `scheduler_workers.rs` (memory mode), executed against a
//! real PostgreSQL server: dual-worker single delivery, expired-lease
//! reclaim + redelivery, duplicate completion rejection, and the timer
//! intermediate catch firing through the server's scheduler + drain wiring.
//!
//! The duplicate-TimerFired injection test is memory-mode-only: it drives the
//! engine in-process (backend-agnostic code path, already covered there), and
//! skipping it here keeps the Docker suite fast.

#![cfg(feature = "postgres")]

use bpm_engine_server_rest::bpm_config::{
    BpmConfig, DatabaseConfig, EngineConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use bpm_engine_worker_sdk::client::ClientError;
use bpm_engine_worker_sdk::EngineClient;
use std::collections::HashMap;
use std::time::Duration;
use testcontainers::clients;
use testcontainers_modules::postgres::Postgres;
use tokio_util::sync::CancellationToken;

/// Process with one ExternalTask (task_type "lease-work").
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

/// Process with a PT1S timer intermediate catch event.
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
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

/// Build the app and serve it on an ephemeral port.
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

/// Start a throwaway PostgreSQL container and build a config for it with a
/// fast timer poll (200 ms) and a permissive rate limit for poll loops.
///
/// Waits until the container actually accepts queries: `build_app` runs
/// migrations on an eager pool connection, and postgres accepts TCP before
/// finishing startup, so an immediate connect can race.
async fn postgres_server_config() -> BpmConfig {
    let docker: &'static clients::Cli = Box::leak(Box::new(clients::Cli::default()));
    let container = docker.run(Postgres::default());
    // Keep the container alive for the whole test process: dropping a
    // testcontainers Container removes it, which would kill the database
    // between this function returning and build_app connecting.
    let container: &'static testcontainers::Container<'static, Postgres> =
        Box::leak(Box::new(container));
    let port = container.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match probe_postgres(&url).await {
            Ok(()) => break,
            Err(e) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = e;
            }
            Err(e) => panic!("postgres container not ready within 30s: {}", e),
        }
    }

    BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "postgres".to_string(),
            url,
            max_connections: 5,
            migrate_on_start: true,
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
    }
}

/// Open a connection and run a trivial query to verify postgres is serving.
async fn probe_postgres(url: &str) -> anyhow::Result<()> {
    let pool = bpm_engine_adapter_postgres::create_pool(url)?;
    let client = pool.get().await?;
    client.execute("SELECT 1", &[]).await?;
    Ok(())
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

async fn fetch(
    worker: &EngineClient,
    worker_id: &str,
    lock_ms: u64,
) -> Result<Vec<bpm_engine_worker_sdk::ExternalTask>, ClientError> {
    worker
        .fetch_and_lock(worker_id, &[LEASE_TASK_TYPE.to_string()], 1, lock_ms)
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

#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_dual_worker_single_delivery() {
    let server = spawn_server(&postgres_server_config().await).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, EXTERNAL_TASK_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "lease-work-flow").await;

    // Worker A and Worker B fetch-and-lock CONCURRENTLY — exactly one wins
    // (FOR UPDATE SKIP LOCKED in the postgres fetch_and_lock transaction).
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

    let (winner, winner_worker, task_id) = if tasks_a.len() == 1 {
        (&worker_a, "worker-a", tasks_a[0].task_id.clone())
    } else {
        (&worker_b, "worker-b", tasks_b[0].task_id.clone())
    };
    winner
        .complete(&task_id, winner_worker, HashMap::new())
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

    let events = history_events(&client, &server.base_url, &instance_id).await;
    assert_eq!(
        count_events(&events, "ExternalTaskLocked", None),
        1,
        "only one worker may lock the task"
    );
    assert_eq!(count_events(&events, "ExternalTaskCompleted", None), 1);
    assert_eq!(count_events(&events, "ProcessCompleted", None), 1);

    server.stop().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_duplicate_completion_is_idempotent() {
    let server = spawn_server(&postgres_server_config().await).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, EXTERNAL_TASK_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "lease-work-flow").await;

    let worker_a = EngineClient::new(&server.base_url);
    let tasks = fetch(&worker_a, "worker-a", 30_000)
        .await
        .expect("worker-a fetch");
    assert_eq!(tasks.len(), 1);
    let task_id = tasks[0].task_id.clone();

    worker_a
        .complete(&task_id, "worker-a", HashMap::new())
        .await
        .expect("first complete should succeed");
    let after_first = wait_for_state(
        &client,
        &server.base_url,
        &instance_id,
        "COMPLETED",
        Duration::from_secs(15),
    )
    .await;

    // Duplicate complete of the already-completed task: 4xx (TaskNotLocked),
    // and the instance / history are unchanged — no double advance.
    let err = worker_a
        .complete(&task_id, "worker-a", HashMap::new())
        .await
        .expect_err("duplicate complete must be rejected");
    let status = assert_client_error_4xx(&err);
    assert_eq!(status, 400, "duplicate complete maps to HTTP 400");

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
        "exactly one ExternalTaskCompleted transition expected"
    );
    assert_eq!(count_events(&events, "ProcessCompleted", None), 1);
    assert_eq!(count_events(&events, "TokenArrived", Some("end")), 1);
    assert_eq!(count_events(&events, "TokenFailed", None), 0);

    server.stop().await;
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_expired_lock_is_reclaimed_and_redelivered() {
    let server = spawn_server(&postgres_server_config().await).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, EXTERNAL_TASK_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "lease-work-flow").await;

    // Worker A locks with a 1-second lease and never completes (simulated crash).
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

    // Worker A's late complete is rejected with 4xx: the task is Locked and
    // owned by worker B — the lease-conflict guard (matches memory adapter).
    let err = worker_a
        .complete(&task_id, "worker-a", HashMap::new())
        .await
        .expect_err("late complete by the previous owner must be rejected");
    assert_client_error_4xx(&err);

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

#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_timer_fires_through_service() {
    let server = spawn_server(&postgres_server_config().await).await;
    let client = reqwest::Client::new();
    deploy(&client, &server.base_url, TIMER_BPMN).await;
    let instance_id = start_instance(&client, &server.base_url, "timer-catch-flow").await;

    // The instance waits at the timer catch event (timer record persisted).
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

    // The scheduler (200 ms poll) + drain task fire the PT1S timer over the
    // postgres timer store and drive the instance to Completed.
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
