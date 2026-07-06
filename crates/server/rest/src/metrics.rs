//! Prometheus metrics endpoint for the BPM engine REST server.
//!
//! Exposes engine metrics via `GET /metrics` in Prometheus text format.
//! Enable with `--features observability`.
//!
//! Metrics are defined and registered in the root `bpm-engine` crate (`src/metrics.rs`).
//! This module only handles the HTTP endpoint wiring.

use axum::response::IntoResponse;

/// Renders the current Prometheus metrics snapshot.
pub type MetricsRenderer = Box<dyn Fn() -> String + Send + Sync>;

/// Initializes Prometheus metrics using the root crate's exporter.
pub fn init_metrics() -> MetricsRenderer {
    let handle = bpm_engine::metrics::install_prometheus_exporter();
    Box::new(move || handle.render())
}

/// Axum handler for `GET /metrics` — returns Prometheus text format.
pub async fn metrics_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<super::state::AppState>>,
) -> impl IntoResponse {
    let body = (state.metrics_render)();
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}
