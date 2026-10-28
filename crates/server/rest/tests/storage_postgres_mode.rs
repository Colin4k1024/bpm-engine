//! Restart acceptance test for the PostgreSQL storage backend (issue E1-1).
//!
//! Requires Docker (testcontainers). Run with:
//!
//! ```text
//! cargo test -p bpm-engine-server-rest --test storage_postgres_mode -- --ignored
//! ```
//!
//! Verifies the acceptance criterion: after starting a second app instance
//! against the same database (true restart with a fresh pool and state),
//! the same process instance is still queryable with its waiting token,
//! the deployed definition is still listed as active, and `/ready` reports
//! the database pool check.

#![cfg(feature = "postgres")]

use bpm_engine_server_rest::bpm_config::{BpmConfig, DatabaseConfig, ServerConfig};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use testcontainers::clients;
use testcontainers_modules::postgres::Postgres;
use tokio_util::sync::CancellationToken;

/// BPMN with a UserTask so the started instance blocks (stays Running).
const USER_TASK_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="restart-probe" isExecutable="true">
    <startEvent id="start"/>
    <userTask id="review" name="Review"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="review"/>
    <sequenceFlow id="f2" sourceRef="review" targetRef="end"/>
  </process>
</definitions>"#;

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
    /// Stop the server: cancel background tasks, abort the serve task
    /// (which drops the router and the app state).
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        // Give background tasks a moment to observe the cancellation.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn postgres_config(url: &str) -> BpmConfig {
    BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "postgres".to_string(),
            url: url.to_string(),
            max_connections: 5,
        },
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "requires Docker"]
async fn postgres_mode_survives_restart() {
    // Start a throwaway PostgreSQL container (same setup as the adapter
    // integration tests in crates/adapters/postgres/tests/integration.rs).
    let docker: &'static clients::Cli = Box::leak(Box::new(clients::Cli::default()));
    let container = docker.run(Postgres::default());
    let port = container.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);
    let config = postgres_config(&url);

    let client = reqwest::Client::new();

    // --- App 1: deploy definition + start instance -------------------------
    let app1 = spawn_server(&config).await;

    let resp = client
        .post(format!(
            "{}/api/v1/process-definitions/deploy",
            app1.base_url
        ))
        .header("content-type", "application/xml")
        .body(USER_TASK_BPMN.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "deploy should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["process_definition_id"], "restart-probe");

    let resp = client
        .post(format!("{}/api/v1/process-instances", app1.base_url))
        .json(&serde_json::json!({ "process_def_id": "restart-probe" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "start should return 201");
    let body: serde_json::Value = resp.json().await.unwrap();
    let instance_id = body["instance_id"]
        .as_str()
        .expect("instance_id in response")
        .to_string();

    // The instance blocks at the user task: Running, token waiting.
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            app1.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "RUNNING");

    // Readiness includes the db_pool check wired from the postgres pool.
    let resp = client
        .get(format!("{}/ready", app1.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["checks"].get("db_pool").is_some(),
        "/ready should include the db_pool check: {:?}",
        body
    );
    assert_eq!(body["checks"]["db_pool"], "ok");

    // --- Stop app 1 (cancel timer token, abort serve task, drop state) ------
    app1.stop().await;

    // --- App 2: fresh pool and app state (true restart) ---------------------
    // Storage::postgres runs migrate() again — it must be idempotent.
    let app2 = spawn_server(&config).await;

    // The same instance id is still queryable: Running, token still waiting.
    let resp = client
        .get(format!(
            "{}/api/v1/process-instances/{}",
            app2.base_url, instance_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "RUNNING");
    assert_eq!(body["process_def_id"], "restart-probe");
    let tokens = body["tokens"].as_array().expect("tokens array");
    assert!(
        !tokens.is_empty(),
        "instance should still hold its waiting token"
    );
    assert!(
        tokens
            .iter()
            .any(|t| t["node_id"] == "review" && t["status"] == "Waiting"),
        "token should still wait at 'review': {:?}",
        tokens
    );

    // The deployed definition survives the restart and is still active.
    let resp = client
        .get(format!("{}/api/v1/process-definitions", app2.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let defs = body["definitions"].as_array().expect("definitions array");
    let probe = defs
        .iter()
        .find(|d| d["key"] == "restart-probe")
        .expect("restart-probe definition survives the restart");
    assert_eq!(probe["status"], "active");

    // Invariants return 200 after the restart with nothing worse than Info
    // (the engine creates instances with version 0, an Info-level finding).
    let resp = client
        .get(format!("{}/api/v1/invariants/check", app2.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let severe: Vec<&str> = body["violations"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .filter(|v| v["severity"] != "Info")
                .map(|v| v["description"].as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        severe.is_empty(),
        "invariants should have no Warning/Critical violations: {:?}",
        severe
    );

    app2.stop().await;
}
