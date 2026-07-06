//! BPM Engine REST server. Run: cargo run -p bpm-server-rest

mod bpm_config;
mod middleware;
#[cfg(feature = "observability")]
mod metrics;
mod otel;
mod replay;
mod routes;
mod state;

use crate::bpm_config::BpmConfig;
use crate::routes::router;
use crate::state::AppState;
use bpm_engine_adapter_memory::{DeadLetterRepo, MemoryInvariantChecker, MemoryRepo, ProcessDefStore};
use bpm_engine_core::{Node, NodeType, OutgoingEdge, ProcessDefinition};
use bpm_engine_runtime::{
    BpmEngine, CallActivityCompletionHandler, CallActivityStartedHandler,
    ExternalTaskCompletedHandler, HistoryHandler, MessageCatchHandler, ProcessCompletedHandler,
    ProcessStartHandler, SignalCatchHandler, TimerFiredHandler, TokenArrivedHandler,
    UserTaskCompletedHandler,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

fn minimal_process() -> ProcessDefinition {
    ProcessDefinition {
        id: "minimal",
        start: "start",
        boundary_events: HashMap::new(),
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "end",
                        condition: None,
                    }],
                },
            ),
            (
                "end",
                Node {
                    id: "end",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
        ]),
    }
}

/// Process with ExternalTask (task_type = "payment") for worker-sdk payment example.
fn payment_process() -> ProcessDefinition {
    ProcessDefinition {
        id: "payment-flow",
        start: "start",
        boundary_events: HashMap::new(),
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "payment",
                        condition: None,
                    }],
                },
            ),
            (
                "payment",
                Node {
                    id: "payment",
                    node_type: NodeType::ExternalTask {
                        task_type: "payment".to_string(),
                        retries: 3,
                        timeout_secs: 60,
                    },
                    outgoing_edges: vec![OutgoingEdge {
                        target: "end",
                        condition: None,
                    }],
                },
            ),
            (
                "end",
                Node {
                    id: "end",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
        ]),
    }
}

/// Wait for a shutdown signal (SIGTERM or SIGINT/Ctrl-C).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { info!("received SIGINT (Ctrl-C), starting shutdown"); }
        _ = terminate => { info!("received SIGTERM, starting shutdown"); }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Load configuration (file + env + defaults)
    let config = BpmConfig::load();

    // Initialize structured tracing (with optional OTel integration)
    otel::init_tracing(&config.log.level, &config.log.format);

    // Print effective config (secrets masked)
    config.log_effective();

    // Sync env vars from config so downstream code (middleware) can read them
    if !config.auth.jwt_secret.is_empty() {
        std::env::set_var("BPM_JWT_SECRET", &config.auth.jwt_secret);
    }
    if !config.auth.api_key.is_empty() {
        std::env::set_var("BPM_API_KEY", &config.auth.api_key);
    }

    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(minimal_process());
    def_store.register(payment_process());

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(ProcessCompletedHandler),
        Box::new(CallActivityStartedHandler),
        Box::new(CallActivityCompletionHandler),
        Box::new(MessageCatchHandler),
        Box::new(SignalCatchHandler),
        Box::new(UserTaskCompletedHandler),
        Box::new(ExternalTaskCompletedHandler),
        Box::new(TimerFiredHandler),
        Box::new(HistoryHandler),
    ]);

    // Shared cancellation token for the timer scheduler
    let timer_cancel = CancellationToken::new();

    let dead_letter_store: Arc<dyn bpm_engine_storage::DeadLetterStore> =
        Arc::new(DeadLetterRepo::new());

    let invariant_checker = MemoryInvariantChecker::new(repo.clone());

    // Initialize Prometheus metrics (feature-gated)
    #[cfg(feature = "observability")]
    let metrics_render = metrics::init_metrics();

    let state = Arc::new(AppState {
        engine,
        repo,
        def_store,
        dead_letter_store,
        invariant_checker,
        replay_sessions: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        timer_cancel: timer_cancel.clone(),
        extra_health_checks: None,
        #[cfg(feature = "observability")]
        metrics_render,
    });

    // Start the timer scheduler in the background.
    // Wire the event channel so TimerFired events are processed by the engine.
    let timer_config = config.timer_scheduler_config();
    let (timer_tx, mut timer_rx) = tokio::sync::mpsc::unbounded_channel();
    let _timer_handle = bpm_engine_runtime::spawn_timer_scheduler(
        state.repo.clone() as Arc<dyn bpm_engine_storage::TimerStore>,
        timer_config,
        timer_tx,
    );

    // Spawn a task to process timer events through the engine
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            while let Some(event) = timer_rx.recv().await {
                let mut ctx = crate::routes::build_ctx_for_timer(&state);
                state.engine.run_async(event, &mut ctx).await;
            }
        });
    }

    let rate_limit_rpm = config.rate_limit.requests_per_minute;
    let app = router(state, rate_limit_rpm);

    // Add OTel HTTP trace layer if the feature is enabled
    #[cfg(feature = "otel")]
    let app = app.layer(otel::http_trace_layer());

    let addr = config.server_addr();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(addr = %addr, "BPM Engine REST server listening");

    // Start the server with graceful shutdown support.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Graceful shutdown: stop the timer scheduler
    info!("stopping timer scheduler");
    timer_cancel.cancel();

    // Give the timer scheduler a moment to finish its current poll cycle
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Flush OTel traces before exiting
    otel::shutdown_tracing();

    info!("shutdown complete");
    Ok(())
}
