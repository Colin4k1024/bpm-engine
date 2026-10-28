//! Approval store trait (E5-2, ADR-008): engine-owned, non-forgeable
//! approval records for high-risk external-task steps.
//!
//! # Decision vs. effect (the re-drivable unit)
//!
//! `decide` flips the DECISION durably and leaves the record's effect at
//! `Pending`; the caller applies the effect (task creation on grant, dead
//! letter + token-terminal on reject) and confirms it with
//! [`ApprovalStore::mark_effect_applied`]. A crash between the two is
//! recoverable: a retried same-decision decide re-drives a pending effect
//! before answering, and the server's startup sweep re-drives every record
//! returned by [`ApprovalStore::list_decided_with_pending_effect`].
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use bpm_engine_core::{ApprovalDecision, ApprovalEffect, ApprovalRecord, ApprovalStatus};
//! use bpm_engine_storage::{ApprovalDecideOutcome, ApprovalError, ApprovalStore};
//!
//! # async fn example(repo: Arc<impl ApprovalStore>) -> anyhow::Result<()> {
//! // The engine gate creates a Pending record when a token arrives at an
//! // approval-required node (task-creation-after-approval: no external task
//! // exists yet, so workers never see unapproved work).
//! let record = ApprovalRecord {
//!     id: "appr-1".into(),
//!     instance_id: "inst-1".into(),
//!     node_id: "charge".into(),
//!     token_id: "token-1".into(),
//!     task_type: "payment_charge".into(),
//!     status: ApprovalStatus::Pending,
//!     effect: ApprovalEffect::Pending,
//!     decided_by: None,
//!     decided_at: None,
//!     note: None,
//!     created_at: "1234".into(),
//! };
//! repo.create(&record).await?;
//!
//! // An operator decides — atomically, exactly once. The DECISION is
//! // durable from here on; the EFFECT is still Pending until the caller
//! // applies it and calls mark_effect_applied.
//! match repo
//!     .decide("appr-1", ApprovalDecision::Approve, "alice", Some("ok"))
//!     .await
//!     .map_err(|e| anyhow::anyhow!("{e}"))?
//! {
//!     ApprovalDecideOutcome::Decided(record) => {
//!         assert_eq!(record.status, ApprovalStatus::Approved);
//!         assert_eq!(record.effect, ApprovalEffect::Pending);
//!     }
//!     ApprovalDecideOutcome::AlreadyDecided(record) => {
//!         // A concurrent/retried decide: the ORIGINAL outcome stands.
//!         assert_eq!(record.status, ApprovalStatus::Approved);
//!     }
//! }
//!
//! // The caller applied the effect (created the task): confirm it. A
//! // pending-effect re-drive (retried decide / startup sweep) is what calls
//! // this late.
//! repo.mark_effect_applied("appr-1").await?;
//!
//! // A conflicting second decision never flips the record.
//! let outcome = repo
//!     .decide("appr-1", ApprovalDecision::Reject, "bob", None)
//!     .await
//!     .map_err(|e| anyhow::anyhow!("{e}"))?;
//! assert!(matches!(outcome, ApprovalDecideOutcome::AlreadyDecided(_)));
//!
//! // The startup sweep finds exactly the decisions whose effect is still
//! // pending (none here — the effect above was applied).
//! assert!(repo.list_decided_with_pending_effect().await?.is_empty());
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use bpm_engine_core::{ApprovalDecision, ApprovalRecord};

/// Typed errors for approval operations.
///
/// These errors are safe to expose to API clients without leaking internal
/// details.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    /// The specified approval record was not found.
    #[error("approval not found: {approval_id}")]
    NotFound {
        /// The approval record id that was not found.
        approval_id: String,
    },
    /// An unexpected internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}

/// The outcome of an atomic [`ApprovalStore::decide`] call (E5-2
/// only-once semantics).
#[derive(Debug, Clone)]
pub enum ApprovalDecideOutcome {
    /// This call performed the decision: the record was `Pending` and is
    /// now `Approved`/`Rejected` as requested (with the actor and note).
    Decided(ApprovalRecord),
    /// The record was ALREADY decided — the original outcome stands and is
    /// returned unchanged. A concurrent or retried decide can never flip a
    /// decided record; the caller turns this into a replay (same decision)
    /// or a conflict (opposite decision).
    AlreadyDecided(ApprovalRecord),
}

/// Approval store: persists engine-owned approval records (ADR-008).
///
/// Records are subject-unique on `(instance_id, node_id, token_id)`:
/// [`ApprovalStore::create`] is an idempotent insert — a record for the
/// same subject already exists is KEPT (its id, status, and decision
/// survive), so a re-arrival of the same token at the same node (crash
/// recovery, event re-delivery) never creates a second approval
/// requirement.
///
/// [`ApprovalStore::decide`] is atomic only-once: exactly the first decide
/// flips a `Pending` record; every later call returns
/// [`ApprovalDecideOutcome::AlreadyDecided`] with the ORIGINAL outcome.
#[async_trait]
pub trait ApprovalStore: Send + Sync {
    /// Insert a Pending approval record; idempotent by subject
    /// `(instance_id, node_id, token_id)` — an existing record for the
    /// same subject is kept unchanged (create is a no-op then).
    async fn create(&self, record: &ApprovalRecord) -> anyhow::Result<()>;

    /// Load an approval record by id.
    async fn get(&self, approval_id: &str) -> anyhow::Result<Option<ApprovalRecord>>;

    /// List the approval records of one instance (ordered by creation).
    async fn list_by_instance(&self, instance_id: &str) -> anyhow::Result<Vec<ApprovalRecord>>;

    /// Decide a record: `Pending → Approved|Rejected` exactly once, with
    /// the deciding actor (the E5-1 verified subject) and an optional note.
    ///
    /// A record that is already decided is NEVER changed: the call returns
    /// [`ApprovalDecideOutcome::AlreadyDecided`] carrying the original
    /// outcome (and its original actor/note), so a concurrent double
    /// decide or a network retry cannot produce a second effect.
    ///
    /// The DECISION is durable after this call; the record's effect stays
    /// `Pending` until the caller applies the effect and confirms it with
    /// [`ApprovalStore::mark_effect_applied`] (the decision/effect
    /// re-drivable unit, ADR-008 §7.1).
    async fn decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
        actor: &str,
        note: Option<&str>,
    ) -> Result<ApprovalDecideOutcome, ApprovalError>;

    /// Mark the decided record's effect as APPLIED (idempotent).
    ///
    /// Called by the effect application after the grant/reject effect has
    /// been (fully) applied. Safe to call repeatedly: an already-applied
    /// record stays applied. An unknown id is a no-op (the caller loaded
    /// the record first).
    async fn mark_effect_applied(&self, approval_id: &str) -> anyhow::Result<()>;

    /// Every DECIDED record whose effect is still `Pending` — the startup
    /// sweep's work list (ADR-008 §7.1): decisions whose effect was never
    /// (fully) applied are re-driven at server start, ordered by creation.
    async fn list_decided_with_pending_effect(&self) -> anyhow::Result<Vec<ApprovalRecord>>;
}
