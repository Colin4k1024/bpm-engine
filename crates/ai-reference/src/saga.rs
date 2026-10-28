//! Saga compensation support (E3-4, 补偿): the application-side coordinator
//! over the engine's compensation record store.
//!
//! How compensation works in this engine (kept honest to
//! `crates/core/src/saga.rs` and `tests/saga_compensation.rs`):
//!
//! - The engine **persists** [`CompensationRecordRow`]s through the
//!   [`CompensationRecordRepo`] (wired into every `EngineContext`, including
//!   the REST server's). The event model and history handler can carry and
//!   record `SagaStarted` / `SagaCompleted` if application code submits
//!   them — in v1 no engine code path constructs those events itself. It
//!   does **not** create records or run the loop either: the
//!   saga coordinator lives in the application (the design's "Compensation
//!   over rollback" principle — long-running consistency is coordinated,
//!   not transactional).
//! - The loop is: load the instance's records → keep `Pending` → execute
//!   **newest first** (reverse completion order) → stop at the first
//!   failure (manual intervention territory).
//!
//! [`SagaCoordinator`] is that loop, reusable by embedders and demonstrated
//! end to end by the reference suite: the
//! [`compensation_flow`](crate::compensation_flow) models the saga shape in
//! the DSL itself (failure path → explicit compensating step), and the
//! integration test records the forward steps into a live server's real
//! store and compensates through this coordinator.
//!
//! Note on statuses: the storage trait is append-only (`add` +
//! `list_by_instance`) — there is no update method in v1, matching
//! `tests/saga_compensation.rs` (which tracks outcomes without writing
//! terminal statuses back). [`SagaCoordinator::compensate`] therefore
//! reports what ran through its returned [`CompensationOutcome`]s; a future
//! engine release may add an update method, at which point the coordinator
//! can persist terminal statuses without changing its callers.

use std::cmp::Reverse;

use bpm_engine_storage::{CompensationRecordRepo, CompensationRecordRow};

use crate::runner::FlowOutcome;

/// The compensation flow's forward (compensatable) steps, in completion
/// order. The compensation step undoes them in REVERSE order.
pub const SAGA_FORWARD_STEPS: [&str; 2] = ["reserve", "charge"];

/// Status of a record awaiting compensation.
pub const STATUS_PENDING: &str = "Pending";

/// Status of a successfully compensated record.
pub const STATUS_COMPLETED: &str = "Completed";

/// Status of a record whose compensation failed.
pub const STATUS_FAILED: &str = "Failed";

/// Result of one compensation invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompensationOutcome {
    /// The undo handler for this node ran successfully.
    Completed {
        /// The compensated node.
        node_id: String,
    },
    /// The undo handler failed; the loop stopped here (older records were
    /// NOT compensated and need manual intervention).
    Failed {
        /// The node whose compensation failed.
        node_id: String,
        /// The failure reason.
        reason: String,
    },
}

/// The application-side saga coordinator: record compensatable steps as
/// they complete, then compensate them newest-first.
///
/// Unitary (no state) — every method works directly against the engine's
/// store, so one coordinator can serve any number of instances.
#[derive(Debug, Clone, Copy, Default)]
pub struct SagaCoordinator;

impl SagaCoordinator {
    /// Record one completed forward step as a `Pending` compensation record
    /// (`order` = completion order; higher = later = compensated first).
    ///
    /// `created_at` is a deterministic order-derived stamp: the reference
    /// suite uses no clock, and the v1 store treats the field opaquely.
    pub async fn record(
        repo: &dyn CompensationRecordRepo,
        instance_id: &str,
        node_id: &str,
        handler_ref: &str,
        order: u32,
    ) -> anyhow::Result<()> {
        let row = CompensationRecordRow {
            id: format!("{instance_id}:{node_id}:{order}"),
            instance_id: instance_id.to_string(),
            node_id: node_id.to_string(),
            handler_ref: handler_ref.to_string(),
            order,
            status: STATUS_PENDING.to_string(),
            created_at: format!("saga-{order}"),
        };
        repo.add(&row)
            .await
            .map_err(|e| anyhow::anyhow!("compensation record add failed: {e}"))
    }

    /// Record every `forward_node` that actually executed in `outcome` (per
    /// its step trace), in the given order — the "application observes step
    /// completion → records compensation" bridge.
    ///
    /// Nodes without an executed step are skipped, so partially-run flows
    /// only record the work that really completed.
    pub async fn record_from_steps(
        repo: &dyn CompensationRecordRepo,
        outcome: &FlowOutcome,
        forward_nodes: &[&str],
    ) -> anyhow::Result<()> {
        for (index, node) in forward_nodes.iter().enumerate() {
            if outcome.step(node).is_some_and(|step| step.executed()) {
                Self::record(
                    repo,
                    &outcome.instance_id,
                    node,
                    &format!("undo-{node}"),
                    (index + 1) as u32,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Run the compensation loop for one instance: load its records, keep
    /// `Pending`, invoke `undo` newest-first, stop at the first failure.
    ///
    /// `undo` receives each record and reports success (`Ok(())`) or a
    /// failure reason; the executed order is returned as
    /// [`CompensationOutcome`]s.
    pub async fn compensate(
        repo: &dyn CompensationRecordRepo,
        instance_id: &str,
        mut undo: impl FnMut(&CompensationRecordRow) -> Result<(), String>,
    ) -> anyhow::Result<Vec<CompensationOutcome>> {
        let records = repo.list_by_instance(instance_id).await;
        let mut pending: Vec<CompensationRecordRow> = records
            .into_iter()
            .filter(|row| row.status == STATUS_PENDING)
            .collect();
        // Newest (highest order) first: reverse completion order.
        pending.sort_by_key(|row| Reverse(row.order));

        let mut outcomes = Vec::new();
        for record in &pending {
            match undo(record) {
                Ok(()) => outcomes.push(CompensationOutcome::Completed {
                    node_id: record.node_id.clone(),
                }),
                Err(reason) => {
                    outcomes.push(CompensationOutcome::Failed {
                        node_id: record.node_id.clone(),
                        reason,
                    });
                    // First failure stops the chain (engine saga semantics).
                    break;
                }
            }
        }
        Ok(outcomes)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Minimal in-memory repo (the integration test uses the engine's real
    /// memory store instead).
    #[derive(Default)]
    struct FakeRepo {
        rows: std::sync::Mutex<Vec<CompensationRecordRow>>,
    }

    #[async_trait::async_trait]
    impl CompensationRecordRepo for FakeRepo {
        async fn add(&self, record: &CompensationRecordRow) -> anyhow::Result<()> {
            self.rows.lock().unwrap().push(record.clone());
            Ok(())
        }

        async fn list_by_instance(&self, instance_id: &str) -> Vec<CompensationRecordRow> {
            let mut rows: Vec<CompensationRecordRow> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.instance_id == instance_id)
                .cloned()
                .collect();
            rows.sort_by_key(|r| r.order);
            rows
        }
    }

    fn repo() -> Arc<FakeRepo> {
        Arc::new(FakeRepo::default())
    }

    #[tokio::test]
    async fn records_are_pending_and_sorted_ascending() {
        let repo = repo();
        SagaCoordinator::record(repo.as_ref(), "inst-1", "reserve", "undo-reserve", 1)
            .await
            .unwrap();
        SagaCoordinator::record(repo.as_ref(), "inst-1", "charge", "undo-charge", 2)
            .await
            .unwrap();
        let rows = repo.list_by_instance("inst-1").await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].node_id, "reserve");
        assert_eq!(rows[0].order, 1);
        assert_eq!(rows[1].node_id, "charge");
        // Instances are isolated.
        assert!(repo.list_by_instance("other").await.is_empty());
    }

    #[tokio::test]
    async fn compensate_runs_newest_first() {
        let repo = repo();
        SagaCoordinator::record(repo.as_ref(), "inst-2", "reserve", "undo-reserve", 1)
            .await
            .unwrap();
        SagaCoordinator::record(repo.as_ref(), "inst-2", "charge", "undo-charge", 2)
            .await
            .unwrap();

        let mut undone: Vec<String> = Vec::new();
        let outcomes = SagaCoordinator::compensate(repo.as_ref(), "inst-2", |record| {
            undone.push(record.node_id.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(undone, ["charge", "reserve"], "newest first");
        assert_eq!(
            outcomes,
            vec![
                CompensationOutcome::Completed {
                    node_id: "charge".to_string()
                },
                CompensationOutcome::Completed {
                    node_id: "reserve".to_string()
                },
            ]
        );
    }

    #[tokio::test]
    async fn compensate_stops_at_first_failure() {
        let repo = repo();
        for (node, order) in [("reserve", 1u32), ("charge", 2), ("ship", 3)] {
            SagaCoordinator::record(
                repo.as_ref(),
                "inst-3",
                node,
                &format!("undo-{node}"),
                order,
            )
            .await
            .unwrap();
        }
        let outcomes = SagaCoordinator::compensate(repo.as_ref(), "inst-3", |record| {
            if record.node_id == "charge" {
                Err("undo-payment-failed".to_string())
            } else {
                Ok(())
            }
        })
        .await
        .unwrap();
        // ship (order 3) succeeded, charge (order 2) failed -> stop:
        // reserve (order 1) was never attempted.
        assert_eq!(outcomes.len(), 2);
        assert_eq!(
            outcomes[0],
            CompensationOutcome::Completed {
                node_id: "ship".to_string()
            }
        );
        assert_eq!(
            outcomes[1],
            CompensationOutcome::Failed {
                node_id: "charge".to_string(),
                reason: "undo-payment-failed".to_string()
            }
        );
    }

    #[tokio::test]
    async fn compensate_skips_non_pending_records_and_empty_instances() {
        let repo = repo();
        // A Completed record is not compensated again.
        repo.add(&CompensationRecordRow {
            id: "r1".into(),
            instance_id: "inst-4".into(),
            node_id: "done".into(),
            handler_ref: "undo-done".into(),
            order: 1,
            status: STATUS_COMPLETED.into(),
            created_at: "saga-1".into(),
        })
        .await
        .unwrap();
        let outcomes = SagaCoordinator::compensate(repo.as_ref(), "inst-4", |_| Ok(()))
            .await
            .unwrap();
        assert!(outcomes.is_empty());
        // Unknown instance: nothing.
        let outcomes = SagaCoordinator::compensate(repo.as_ref(), "missing", |_| Ok(()))
            .await
            .unwrap();
        assert!(outcomes.is_empty());
    }

    #[tokio::test]
    async fn record_from_steps_follows_the_step_trace() {
        let repo = repo();
        let mut outcome = FlowOutcome::for_tests("inst-5", "COMPLETED");
        outcome.steps = vec![
            crate::StepSummary::for_tests(1, "start", "node", vec!["ProcessStarted".into()]),
            crate::StepSummary::for_tests(
                2,
                "reserve",
                "external_task",
                vec!["TokenArrived".into(), "ExternalTaskCompleted".into()],
            ),
            crate::StepSummary::for_tests(
                3,
                "charge",
                "external_task",
                vec!["TokenArrived".into()],
            ),
        ];
        // charge has no completion event: not recorded.
        SagaCoordinator::record_from_steps(repo.as_ref(), &outcome, &SAGA_FORWARD_STEPS)
            .await
            .unwrap();
        let rows = repo.list_by_instance("inst-5").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].node_id, "reserve");
        assert_eq!(rows[0].handler_ref, "undo-reserve");
    }
}
