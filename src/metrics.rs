//! Engine metrics for the `observability` feature flag.
//!
//! Provides Prometheus-compatible counters and histograms for key engine operations.
//! Enable with `cargo build --features observability`.

use metrics::{counter, histogram};
use std::time::Instant;

/// Record that a process instance was started.
pub fn record_process_started() {
    counter!("bpm_engine_processes_started_total").increment(1);
}

/// Record that a process instance completed.
pub fn record_process_completed() {
    counter!("bpm_engine_processes_completed_total").increment(1);
}

/// Record that a token arrived at a node.
pub fn record_token_arrived(node_type: &str) {
    counter!("bpm_engine_tokens_arrived_total", "node_type" => node_type.to_string()).increment(1);
}

/// Record that a token completed.
pub fn record_token_completed() {
    counter!("bpm_engine_tokens_completed_total").increment(1);
}

/// Record that a token failed.
pub fn record_token_failed() {
    counter!("bpm_engine_tokens_failed_total").increment(1);
}

/// Record that a timer was fired by the scheduler.
pub fn record_timer_fired() {
    counter!("bpm_engine_timers_fired_total").increment(1);
}

/// Record that an external task was created.
pub fn record_external_task_created(task_type: &str) {
    counter!("bpm_engine_external_tasks_created_total", "task_type" => task_type.to_string())
        .increment(1);
}

/// Record that an external task was completed by a worker.
pub fn record_external_task_completed(task_type: &str) {
    counter!("bpm_engine_external_tasks_completed_total", "task_type" => task_type.to_string())
        .increment(1);
}

/// Record that an external task failed.
pub fn record_external_task_failed(task_type: &str) {
    counter!("bpm_engine_external_tasks_failed_total", "task_type" => task_type.to_string())
        .increment(1);
}

/// Record the duration of an engine event pump cycle.
pub fn record_event_pump_duration(start: Instant) {
    let duration = start.elapsed().as_secs_f64();
    histogram!("bpm_engine_event_pump_duration_seconds").record(duration);
}

/// Install the Prometheus exporter and return the handle for serving `/metrics`.
///
/// Call once at application startup. The returned `PrometheusHandle` can render
/// the metrics endpoint content via `handle.render()`.
pub fn install_prometheus_exporter() -> metrics_exporter_prometheus::PrometheusHandle {
    let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
    builder
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}
