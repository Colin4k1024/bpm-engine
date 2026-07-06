//! Persistent timer scheduler (ADR-004).
//! Spawns a background `tokio::spawn` task that polls `TimerStore::list_due()`
//! at a configurable interval and sends `TimerFired` events via a channel.

use bpm_engine_core::{payloads, EngineEvent};
use bpm_engine_storage::TimerStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

/// Configuration for the background timer polling loop.
pub struct TimerSchedulerConfig {
    /// How often to query for due timers (default: 1 second).
    pub poll_interval: Duration,
    /// Maximum timers to fire per poll cycle (default: 100).
    pub batch_size: u32,
}

impl Default for TimerSchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            batch_size: 100,
        }
    }
}

/// Handle returned by [`spawn_timer_scheduler`] for graceful shutdown.
pub struct TimerSchedulerHandle {
    cancel: CancellationToken,
}

impl TimerSchedulerHandle {
    /// Signal the scheduler to stop. The background task exits after its current poll.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

/// Spawn the timer scheduler as a background tokio task.
///
/// Performs an initial sweep on startup (crash recovery) then polls at `config.poll_interval`.
/// Fired timers are sent as [`EngineEvent::TimerFired`] through the provided channel.
pub fn spawn_timer_scheduler(
    timer_store: Arc<dyn TimerStore>,
    config: TimerSchedulerConfig,
    event_tx: mpsc::UnboundedSender<EngineEvent>,
) -> TimerSchedulerHandle {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        info!(
            poll_interval_ms = config.poll_interval.as_millis(),
            "timer scheduler started"
        );

        // Initial sweep for timers that accumulated during downtime
        poll_and_fire(&timer_store, &event_tx, config.batch_size).await;

        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    info!("timer scheduler shutting down");
                    break;
                }
                _ = tokio::time::sleep(config.poll_interval) => {
                    poll_and_fire(&timer_store, &event_tx, config.batch_size).await;
                }
            }
        }
    });

    TimerSchedulerHandle { cancel }
}

async fn poll_and_fire(
    timer_store: &Arc<dyn TimerStore>,
    event_tx: &mpsc::UnboundedSender<EngineEvent>,
    batch_size: u32,
) {
    let now = chrono_now_iso();
    match timer_store.list_due(&now, batch_size).await {
        Ok(timers) => {
            if !timers.is_empty() {
                debug!(count = timers.len(), "firing due timers");
            }
            for timer in timers {
                if let Err(e) = timer_store.mark_fired(&timer.id).await {
                    error!(timer_id = %timer.id, error = %e, "failed to mark timer fired");
                    #[cfg(feature = "observability")]
                    metrics::counter!("bpm_engine_errors_total").increment(1);
                    continue;
                }
                #[cfg(feature = "observability")]
                metrics::counter!("bpm_engine_timers_fired_total").increment(1);
                let event = EngineEvent::TimerFired(payloads::TimerFired {
                    timer_id: timer.id,
                    token_id: timer.token_id,
                    node_id: timer.node_id,
                });
                if event_tx.send(event).is_err() {
                    error!("timer event channel closed");
                    return;
                }
            }
        }
        Err(e) => {
            error!(error = %e, "failed to list due timers");
        }
    }
}

fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    secs.to_string()
}
