//! REST-level tests for the typed 401/429 middleware errors (E2-4).
//!
//! The auth tests configure `auth.api_key`, which makes `build_app` set
//! `BPM_API_KEY` for the whole test PROCESS — every test in this file
//! therefore runs with auth enabled (tests in other files run in their
//! own processes and are unaffected). Memory driver, default CI.
//!
//! Body contract (additive, E2-4): the previous bodies carried only
//! `error`; they now also carry the stable `code` — `unauthorized` (401)
//! and `rate_limited` (429).

use bpm_engine_server_rest::bpm_config::{
    AuthConfig, BpmConfig, DatabaseConfig, RateLimitConfig, ServerConfig,
};
use bpm_engine_server_rest::build_app;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

struct TestServer {
    base_url: String,
    serve_task: tokio::task::JoinHandle<()>,
    timer_cancel: CancellationToken,
}

fn config(api_key: &str, requests_per_minute: u64) -> BpmConfig {
    BpmConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        database: DatabaseConfig {
            driver: "memory".to_string(),
            ..Default::default()
        },
        auth: AuthConfig {
            jwt_secret: String::new(),
            api_key: api_key.to_string(),
        },
        rate_limit: RateLimitConfig {
            requests_per_minute,
        },
        ..Default::default()
    }
}

async fn spawn_server(config: &BpmConfig) -> TestServer {
    let app = build_app(config).await.expect("build_app failed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let timer_cancel = app.timer_cancel.clone();
    let router = app.router.clone();
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    TestServer {
        base_url: format!("http://{}", addr),
        serve_task,
        timer_cancel,
    }
}

impl TestServer {
    async fn stop(self) {
        self.timer_cancel.cancel();
        self.serve_task.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn unauthorized_rejections_carry_the_stable_code() {
    // Generous rate limit: only the auth behavior is under test here.
    let server = spawn_server(&config("secret-key", 100_000)).await;
    let client = reqwest::Client::new();

    // Missing key.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "missing or invalid API key");
    assert_eq!(body["code"], "unauthorized");

    // Wrong key.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "unauthorized");

    // Correct key passes.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", "secret-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    server.stop().await;
}

#[tokio::test]
async fn rate_limited_rejections_carry_the_stable_code() {
    // One request per minute: the first request passes, the second is
    // throttled. The limiter is the OUTERMOST layer, so it also counts
    // authenticated requests.
    let server = spawn_server(&config("secret-key", 1)).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", "secret-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "first request is allowed");

    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .header("X-API-Key", "secret-key")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        429,
        "second request within the window is throttled"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "rate limit exceeded");
    assert_eq!(body["code"], "rate_limited");

    server.stop().await;
}

#[tokio::test]
async fn capabilities_endpoint_is_unauthenticated() {
    // E4-1 (issue #19): the agent tool catalog is discovery metadata
    // served BEFORE an agent holds an API key (like /health and /ready)
    // — with auth enforced and NO key supplied it must stay reachable,
    // while the engine operations keep rejecting key-less requests.
    let server = spawn_server(&config("secret-key", 100_000)).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/capabilities", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "the catalog is key-less discovery metadata"
    );
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .expect("content-type header");
    assert!(
        content_type.starts_with("application/json"),
        "the catalog must be served as JSON, got {content_type:?}"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["catalog_version"], "1");
    assert!(
        body["tools"].as_array().is_some_and(|t| !t.is_empty()),
        "the served catalog must list tools"
    );

    // The engine operations stay behind the API-key layer.
    let resp = client
        .get(format!("{}/api/v1/process-instances", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    server.stop().await;
}
