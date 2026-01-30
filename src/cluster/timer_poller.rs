//! v3: Timer Poller — run by Leader only; list_due, mark_fired, write TimerFired to outbox (or run engine).

use crate::engine::payloads;
use crate::persistence::{OutboxRepo, TimerRepo};
use tracing::debug;

/// Run one cycle: list due timers, for each: mark_fired, then write TimerFired to outbox
/// so the outbox publisher (or same process) will dispatch it.
/// `timer_repo` and `outbox_repo` must be set (e.g. from EngineContext or passed separately).
/// `tenant_id` for outbox event (optional).
pub fn run_one_cycle(
    timer_repo: &dyn TimerRepo,
    outbox_repo: Option<&dyn OutboxRepo>,
    now_iso: &str,
    tenant_id: Option<&str>,
    limit: u32,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let due = timer_repo.list_due(now_iso, limit)?;
    let mut count = 0usize;
    for t in &due {
        if let Err(e) = timer_repo.mark_fired(&t.id) {
            tracing::warn!(timer_id = %t.id, error = %e, "mark_fired failed");
            continue;
        }
        count += 1;
        let payload = payloads::TimerFired {
            timer_id: t.id.clone(),
            token_id: t.token_id.clone(),
        };
        let payload_json = serde_json::to_string(&payload).unwrap_or_default();
        if let Some(outbox) = outbox_repo {
            let _ = outbox.insert_pending(tenant_id, "TimerFired", &payload_json);
            debug!(timer_id = %t.id, "timer fired -> outbox");
        }
    }
    Ok(count)
}
