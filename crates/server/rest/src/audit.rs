//! Audit & data retention runtime (E5-3, issue #25).
//!
//! Two concerns live here:
//!
//! - **Retention sweeps** — deleting append-only audit records that aged
//!   out of the configured retention window. Coverage is deliberately
//!   NARROW: history events (`HistoryRepo::delete_older_than` by
//!   `occurred_at`) and dead-letter entries (`DeadLetterStore::
//!   delete_older_than` by `created_at`). Instances, tokens, and process
//!   definitions are OPERATIONAL state — deleting a running instance is
//!   destructive and can break crash recovery, so they are NOT covered
//!   (documented non-goal). Replay sessions are in-memory and die with
//!   the process; nothing to sweep.
//! - **The audit endpoint's aggregation helpers** live in
//!   [`crate::routes`] (`get_instance_audit`) because they are plain
//!   read-only joins over the stores, exactly like the step-trace
//!   assembler.
//!
//! The sweep runs once at startup (after the approval-effect recovery
//! sweep) and then every `audit.sweep_interval_secs` (default 3600). It
//! is disabled entirely when `audit.retention_days` is `0` — the safe
//! default: nothing is ever deleted unless an operator opts in.
//!
//! Single-service honesty: the sweep runs in THIS process only. With
//! several engine processes over one database each runs its own sweep —
//! idempotent (DELETE of already-deleted rows is a no-op), at worst
//! redundant.

use crate::state::AppState;
use bpm_engine_storage::{DeadLetterStore, HistoryRepo};
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Seconds per day — the unit of `audit.retention_days`.
const SECS_PER_DAY: u64 = 86_400;

/// Outcome of one retention sweep (E5-3).
#[derive(Debug, Clone, Serialize)]
pub struct SweepOutcome {
    /// Unix-seconds cutoff that was applied (rows strictly older than
    /// this were deleted).
    pub cutoff_epoch_secs: u64,
    /// History events deleted.
    pub history_deleted: u64,
    /// Dead-letter entries deleted.
    pub dead_letters_deleted: u64,
}

/// The retention cutoff (unix seconds) for `retention_days`.
///
/// `None` when retention is disabled (`0` days). A huge retention window
/// saturates to cutoff `0` — "older than the epoch" — which deletes
/// nothing, the correct safe direction.
pub fn retention_cutoff_epoch_secs(retention_days: u64) -> Option<u64> {
    if retention_days == 0 {
        return None;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(now.saturating_sub(retention_days.saturating_mul(SECS_PER_DAY)))
}

/// Run ONE retention sweep against the configured stores.
///
/// Public so tests and embedders can drive it deterministically (the
/// interval task calls the same function). When retention is disabled
/// (`retention_days == 0`) this is a NO-OP that deletes nothing — the
/// safe default.
///
/// Both deletions are store-level `delete_older_than` calls (SQL is
/// parameter-bound in the Postgres adapter); a failure of either store
/// propagates as `Err` (the interval task logs it and retries on the next
/// tick; the startup sweep logs it and continues serving).
pub async fn run_retention_sweep(state: &AppState) -> anyhow::Result<SweepOutcome> {
    let Some(cutoff) = retention_cutoff_epoch_secs(state.retention.retention_days) else {
        // Retention disabled: a no-op sweep that records nothing.
        return Ok(SweepOutcome {
            cutoff_epoch_secs: 0,
            history_deleted: 0,
            dead_letters_deleted: 0,
        });
    };
    let cutoff_str = cutoff.to_string();
    let history_deleted =
        HistoryRepo::delete_older_than(state.storage.history_repo.as_ref(), &cutoff_str).await?;
    let dead_letters_deleted =
        DeadLetterStore::delete_older_than(state.storage.dead_letter_store.as_ref(), &cutoff_str)
            .await?;
    let outcome = SweepOutcome {
        cutoff_epoch_secs: cutoff,
        history_deleted,
        dead_letters_deleted,
    };
    info!(
        retention_days = state.retention.retention_days,
        cutoff_epoch_secs = cutoff,
        history_deleted,
        dead_letters_deleted,
        "retention sweep completed"
    );
    if let Ok(mut guard) = state.retention.last_sweep.lock() {
        *guard = Some(outcome.clone());
    }
    Ok(outcome)
}

/// Spawn the periodic retention sweep task (E5-3).
///
/// Ticks every `interval` and stops when `cancel` is cancelled (the same
/// token that stops the timer scheduler). The FIRST sweep is the caller's
/// responsibility — `build_app` runs it once, awaited, before serving, so
/// the state is converged when the first request arrives (the
/// `tokio::time::interval` semantics would otherwise delay the first
/// periodic tick by a full interval).
pub fn spawn_retention_task(
    state: Arc<AppState>,
    interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if let Err(err) = run_retention_sweep(&state).await {
                        warn!(
                            error = %err,
                            "retention sweep failed (will retry on the next tick)"
                        );
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_is_none_when_retention_disabled() {
        assert_eq!(retention_cutoff_epoch_secs(0), None);
    }

    #[test]
    fn cutoff_is_in_the_past_for_enabled_retention() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for days in [1u64, 30, 365] {
            let cutoff = retention_cutoff_epoch_secs(days).expect("enabled retention has a cutoff");
            assert!(cutoff <= now, "the cutoff must be at or before now");
            assert!(now - cutoff <= days * SECS_PER_DAY);
        }
    }

    #[test]
    fn huge_retention_saturates_to_delete_nothing() {
        // A retention window larger than the age of the unix epoch: the
        // cutoff saturates at 0 (nothing is "older than the epoch"), so
        // the sweep deletes nothing — the safe direction.
        assert_eq!(
            retention_cutoff_epoch_secs(u64::MAX / SECS_PER_DAY),
            Some(0)
        );
    }
}
