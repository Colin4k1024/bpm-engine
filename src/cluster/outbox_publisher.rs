//! v3: Outbox Publisher — run by Leader only; claim_pending, run engine, mark_published.

use crate::engine::{event_from_outbox, BpmEngine, EngineContext};
use tracing::{debug, warn};

/// Run one cycle: if caller is leader, claim up to `batch_size` outbox events,
/// run each through the engine with `ctx`, then mark_published.
/// `ctx` must have outbox_repo, process_repo, token_repo, process_def_repo, etc. set.
/// `tenant_id` = None means all tenants (legacy).
pub fn run_one_cycle(
    engine: &BpmEngine,
    ctx: &mut EngineContext,
    worker_id: &str,
    tenant_id: Option<&str>,
    batch_size: u32,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let claimed = {
        let outbox = match ctx.outbox_repo.as_deref() {
            Some(o) => o,
            None => return Ok(0),
        };
        outbox.claim_pending(worker_id, tenant_id, batch_size)?
    };
    let n = claimed.len();
    for ev in &claimed {
        let event = match event_from_outbox(&ev.event_type, &ev.payload) {
            Some(e) => e,
            None => {
                warn!(id = %ev.id, event_type = %ev.event_type, "outbox event unknown type, marking published");
                if let Some(outbox) = ctx.outbox_repo.as_deref() {
                    let _ = outbox.mark_published(&ev.id);
                }
                continue;
            }
        };
        debug!(id = %ev.id, event_type = %ev.event_type, "outbox dispatch");
        engine.run(event, ctx);
        if let Some(outbox) = ctx.outbox_repo.as_deref() {
            if let Err(e) = outbox.mark_published(&ev.id) {
                warn!(id = %ev.id, error = %e, "mark_published failed");
            }
        }
    }
    Ok(n)
}
