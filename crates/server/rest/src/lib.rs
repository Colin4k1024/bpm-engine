//! BPM Engine REST server library.
//!
//! Provides the axum router, app state assembly, and storage backend
//! configuration (memory or PostgreSQL) for the `api_server` binary.
//! Embedders can call [`build_app`] to get the router, or [`serve`] to run
//! the whole server with graceful shutdown.
//!
//! Run the binary with: `cargo run -p bpm-engine-server-rest`
//!
//! Storage backend is selected via `database.driver` in the configuration:
//! `"memory"` (default, bootstrap definitions `minimal` and `payment-flow`
//! are registered) or `"postgres"` (definitions must be deployed via the
//! REST API; state survives restarts).

pub mod audit;
pub mod bpm_config;
pub mod capabilities;
#[cfg(feature = "observability")]
pub mod metrics;
pub mod middleware;
pub mod otel;
pub mod replay;
pub mod routes;
pub mod state;
pub mod step_trace;
pub mod storage;

use crate::bpm_config::BpmConfig;
use crate::routes::router;
use crate::state::{AppState, ExtraHealthChecks, VariableContractLimits};
use crate::storage::Storage;
use axum::Router;
use bpm_engine_adapter_memory::ProcessDefStore;
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

/// Handle to a built application: shared state, wired router, and a
/// cancellation token for the timer scheduler / timer event drain task.
///
/// Cancel `timer_cancel` (or drop this handle after cancelling) to stop all
/// background tasks owned by [`build_app`].
pub struct AppHandle {
    /// Shared application state (engine, storage bundle, replay sessions).
    pub state: Arc<AppState>,
    /// Fully wired axum router (auth, rate limit, request-id layers included).
    pub router: Router,
    /// Cancellation token for the timer scheduler and the timer event drain
    /// task. Cancelling it stops both background tasks.
    pub timer_cancel: CancellationToken,
}

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

/// Validate the storage driver configuration.
///
/// Errors clearly when the driver is unknown, or when the postgres driver is
/// selected without a URL (or without the `postgres` build feature).
fn validate_storage_config(config: &BpmConfig) -> anyhow::Result<()> {
    match config.database.driver.as_str() {
        "memory" => Ok(()),
        "postgres" => {
            if config.database.url.trim().is_empty() {
                anyhow::bail!("database.url must not be empty when database.driver = \"postgres\"");
            }
            #[cfg(not(feature = "postgres"))]
            anyhow::bail!(
                "database.driver = \"postgres\" requires the `postgres` feature \
                 (rebuild with --features postgres)"
            );
            #[cfg(feature = "postgres")]
            Ok(())
        }
        other => anyhow::bail!(
            "unsupported database.driver {:?}: expected \"memory\" or \"postgres\"",
            other
        ),
    }
}

/// Assemble the storage bundle for the configured driver.
///
/// Memory mode pre-registers the bootstrap definitions (`minimal`,
/// `payment-flow`). Postgres mode applies (or, when
/// `database.migrate_on_start` is `false`, validates — failing startup on
/// pending migrations) the versioned SQL migrations and returns pool
/// readiness checks for `/ready`; programmatic definitions cannot be
/// persisted there, so definitions must be deployed via the REST API.
async fn build_storage(config: &BpmConfig) -> anyhow::Result<(Storage, Option<ExtraHealthChecks>)> {
    match config.database.driver.as_str() {
        "memory" => {
            let def_store = Arc::new(ProcessDefStore::new());
            def_store.register(minimal_process());
            def_store.register(payment_process());
            info!(
                driver = "memory",
                "storage backend: memory (bootstrap definitions: minimal, payment-flow)"
            );
            Ok((Storage::memory_with_def_store(def_store), None))
        }
        "postgres" => {
            #[cfg(feature = "postgres")]
            {
                let (storage, checks) = Storage::postgres(
                    &config.database.url,
                    config.database.max_connections,
                    config.database.migrate_on_start,
                )
                .await?;
                info!(
                    driver = "postgres",
                    max_connections = config.database.max_connections,
                    migrate_on_start = config.database.migrate_on_start,
                    "storage backend: postgres (deploy definitions via the REST API; \
                     programmatic bootstrap definitions are not persisted)"
                );
                Ok((storage, Some(checks)))
            }
            #[cfg(not(feature = "postgres"))]
            {
                anyhow::bail!(
                    "database.driver = \"postgres\" requires the `postgres` feature \
                     (rebuild with --features postgres)"
                )
            }
        }
        other => anyhow::bail!(
            "unsupported database.driver {:?}: expected \"memory\" or \"postgres\"",
            other
        ),
    }
}

/// Build the full application (state + background tasks + router) from config.
///
/// This validates the storage driver, assembles the storage bundle (applying
/// or, when `database.migrate_on_start` is `false`, validating Postgres
/// migrations), starts the timer scheduler and the timer event drain task,
/// and returns the wired router.
///
/// The caller is responsible for serving `AppHandle::router` and for
/// cancelling `AppHandle::timer_cancel` on shutdown.
pub async fn build_app(config: &BpmConfig) -> anyhow::Result<AppHandle> {
    validate_storage_config(config)?;

    // Sync env vars from config so downstream code (middleware) can read them
    if !config.auth.jwt_secret.is_empty() {
        std::env::set_var("BPM_JWT_SECRET", &config.auth.jwt_secret);
    }
    if !config.auth.api_key.is_empty() {
        std::env::set_var("BPM_API_KEY", &config.auth.api_key);
    }

    let (storage, extra_health_checks) = build_storage(config).await?;

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

    // Shared cancellation token for the timer scheduler and drain task
    let timer_cancel = CancellationToken::new();

    // Initialize Prometheus metrics (feature-gated)
    #[cfg(feature = "observability")]
    let metrics_render = metrics::init_metrics();

    let state = Arc::new(AppState {
        engine,
        storage,
        variable_limits: VariableContractLimits::from(&config.engine),
        history_redaction_policy: bpm_engine_core::HistoryRedactionPolicy {
            max_variable_bytes: config.engine.history_variable_max_bytes,
        },
        approval_required_task_types: Arc::new(
            config
                .engine
                .approval_required_task_types
                .iter()
                .cloned()
                .collect(),
        ),
        instance_lifecycle_lock: Arc::new(tokio::sync::Mutex::new(())),
        replay_sessions: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        timer_cancel: timer_cancel.clone(),
        retention: crate::state::RetentionState {
            retention_days: config.audit.retention_days,
            last_sweep: std::sync::Mutex::new(None),
        },
        extra_health_checks,
        #[cfg(feature = "observability")]
        metrics_render,
    });

    // E5-2 (ADR-008 §7.1): approval-effect startup sweep. Re-drive every
    // decided approval record whose effect is still Pending — decisions
    // that survived a crash between the durable decide and the effect
    // application. Meaningful for the persistent (Postgres) driver; the
    // memory store is process-lifetime dev/test state and starts empty, so
    // the sweep is a no-op there by construction. Runs before serving so
    // the state is converged when the first request arrives; per-record
    // failures are logged (the decision stays durable, a retried decide
    // re-drives the effect).
    routes::recover_pending_approval_effects(&state).await;

    // E5-3: retention startup sweep, then the periodic task. Runs once
    // BEFORE serving (an aged-out database is converged by the time the
    // first request arrives); the periodic task re-sweeps every
    // `audit.sweep_interval_secs` and stops with `timer_cancel`. A sweep
    // failure is logged, not fatal (the next tick retries); retention is
    // disabled (retention_days = 0, the default) ⇒ both are no-ops.
    if let Err(err) = crate::audit::run_retention_sweep(&state).await {
        tracing::warn!(
            error = %err,
            "retention startup sweep failed (the periodic task will retry)"
        );
    }
    if config.audit.retention_days > 0 {
        let sweep_state = Arc::clone(&state);
        let sweep_interval =
            std::time::Duration::from_secs(config.audit.sweep_interval_secs.max(1));
        crate::audit::spawn_retention_task(sweep_state, sweep_interval, timer_cancel.clone());
    }

    // Start the timer scheduler in the background.
    // Wire the event channel so TimerFired events are processed by the engine.
    let timer_config = config.timer_scheduler_config();
    let (timer_tx, mut timer_rx) = tokio::sync::mpsc::unbounded_channel();
    let scheduler_handle = bpm_engine_runtime::spawn_timer_scheduler(
        Arc::clone(&state.storage.timer_store),
        timer_config,
        timer_tx,
    );

    // Also stop the scheduler when the app is cancelled
    {
        let cancel = timer_cancel.clone();
        tokio::spawn(async move {
            cancel.cancelled().await;
            scheduler_handle.shutdown();
        });
    }

    // Spawn a task to process timer events through the engine.
    // Exits when the app cancellation token is cancelled.
    {
        let state = Arc::clone(&state);
        let cancel = timer_cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    event = timer_rx.recv() => {
                        let Some(event) = event else { break };
                        let mut ctx = routes::build_ctx_for_timer(&state);
                        state.engine.run_async(event, &mut ctx).await;
                    }
                }
            }
        });
    }

    let app = router(Arc::clone(&state), config.rate_limit.requests_per_minute);

    Ok(AppHandle {
        state,
        router: app,
        timer_cancel,
    })
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

/// Build the application, bind `config.server_addr()`, and serve until a
/// shutdown signal (SIGINT/SIGTERM) arrives.
///
/// On shutdown the timer scheduler and the timer event drain task are
/// cancelled, and OTel traces are flushed.
pub async fn serve(config: BpmConfig) -> anyhow::Result<()> {
    let app = build_app(&config).await?;

    let addr = config.server_addr();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(addr = %addr, "BPM Engine REST server listening");

    // Add OTel HTTP trace layer if the feature is enabled
    let router = app.router;
    #[cfg(feature = "otel")]
    let router = router.layer(otel::http_trace_layer());

    // Start the server with graceful shutdown support.
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Graceful shutdown: stop the timer scheduler and drain task
    info!("stopping timer scheduler");
    app.timer_cancel.cancel();

    // Give the timer scheduler a moment to finish its current poll cycle
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Flush OTel traces before exiting
    otel::shutdown_tracing();

    info!("shutdown complete");
    Ok(())
}
