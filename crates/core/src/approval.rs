//! Engine-owned approval records for high-risk steps (E5-2, ADR-008).
//!
//! An [`ApprovalRecord`] is persisted engine state — NOT a task variable —
//! created when a token arrives at an approval-required external task node.
//! The external task itself is created only AFTER the record is approved
//! (task-creation-after-approval), so workers never see unapproved work and
//! the worker-side `__approval` gate reads a value the ENGINE wrote.
//!
//! ## Decision vs. effect (re-drivable unit)
//!
//! A `decide` writes the DECISION durably (status `Approved`/`Rejected`) and
//! leaves the record's [`ApprovalEffect`] at `Pending`; the EFFECT of the
//! decision (task creation on grant; dead letter + token-terminal on reject)
//! is applied immediately after and flips the effect to `Applied` on
//! success. A crash or error between the two is recoverable by
//! construction: the decision is durable truth, the effect is idempotent,
//! and a pending effect is re-driven by a retried same-decision `decide`
//! and by the server's startup sweep (ADR-008 §7.1).
//!
//! Pure logic only: no I/O, no async, no storage traits (the
//! `ApprovalStore` trait lives in `bpm-engine-storage`).

/// Lifecycle state of an approval record.
///
/// A record starts as `Pending` when the token arrives at an
/// approval-required node. The first `decide` flips it to `Approved` or
/// `Rejected` — atomically, exactly once (a second decide never changes it).
///
/// # Example
///
/// ```
/// use bpm_engine_core::ApprovalStatus;
///
/// let status = ApprovalStatus::Pending;
/// assert_ne!(status, ApprovalStatus::Approved);
/// assert_ne!(status, ApprovalStatus::Rejected);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalStatus {
    /// Waiting for an operator decision; the token waits, no task exists.
    Pending,
    /// Approved; the external task was (or will be) created fetchable.
    Approved,
    /// Rejected; the token is terminated and the step dead-lettered.
    Rejected,
}

impl ApprovalStatus {
    /// The storage/serialization string of the status.
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "Pending",
            ApprovalStatus::Approved => "Approved",
            ApprovalStatus::Rejected => "Rejected",
        }
    }

    /// Parse a storage string; unknown values yield `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Pending" => Some(ApprovalStatus::Pending),
            "Approved" => Some(ApprovalStatus::Approved),
            "Rejected" => Some(ApprovalStatus::Rejected),
            _ => None,
        }
    }
}

/// Whether the EFFECT of a decided approval record has been applied (E5-2,
/// ADR-008 §7.1 — the decision/effect recoverable unit).
///
/// `decide` flips the decision (`status`) durably but leaves `effect` at
/// `Pending`; the effect application (task creation on grant; dead letter +
/// token-terminal on reject) marks it `Applied` on success. A pending effect
/// on a decided record means "apply (or re-apply) the effect": a retried
/// same-decision decide re-drives it before answering, and the server's
/// startup sweep re-drives every decided record whose effect is still
/// pending. The effect itself is idempotent, so a re-drive after a partial
/// application never duplicates it.
///
/// An UNdecided record (`status: Pending`) trivially carries `effect:
/// Pending` — there is no decision to apply yet.
///
/// # Example
///
/// ```
/// use bpm_engine_core::ApprovalEffect;
///
/// let effect = ApprovalEffect::Pending;
/// assert_ne!(effect, ApprovalEffect::Applied);
/// assert_eq!(ApprovalEffect::parse(effect.as_str()), Some(effect));
/// assert_eq!(ApprovalEffect::parse("applied"), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalEffect {
    /// The decision's effect has not been applied (or not fully) — re-drive
    /// it. This is also the initial value of an undecided record.
    Pending,
    /// The decision's effect has been applied: the task exists (grant) or
    /// the step is dead-lettered and the token terminal (reject).
    Applied,
}

impl ApprovalEffect {
    /// The storage/serialization string of the effect status.
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalEffect::Pending => "Pending",
            ApprovalEffect::Applied => "Applied",
        }
    }

    /// Parse a storage string; unknown values yield `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Pending" => Some(ApprovalEffect::Pending),
            "Applied" => Some(ApprovalEffect::Applied),
            _ => None,
        }
    }
}

/// The decision an operator submits for a pending approval record.
///
/// # Example
///
/// ```
/// use bpm_engine_core::ApprovalDecision;
///
/// assert_eq!(ApprovalDecision::Approve.as_str(), "approve");
/// assert_eq!(ApprovalDecision::Reject.as_str(), "reject");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Grant the step: create the external task (with `__approval=true`).
    Approve,
    /// Deny the step: terminate the token and dead-letter it.
    Reject,
}

impl ApprovalDecision {
    /// The wire string of the decision (the REST `decision` field value).
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalDecision::Approve => "approve",
            ApprovalDecision::Reject => "reject",
        }
    }

    /// Parse the REST `decision` field; anything else is `None` (the route
    /// answers 400 `invalid_request`).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "approve" => Some(ApprovalDecision::Approve),
            "reject" => Some(ApprovalDecision::Reject),
            _ => None,
        }
    }

    /// The [`ApprovalStatus`] this decision produces on the record.
    pub fn resulting_status(&self) -> ApprovalStatus {
        match self {
            ApprovalDecision::Approve => ApprovalStatus::Approved,
            ApprovalDecision::Reject => ApprovalStatus::Rejected,
        }
    }
}

/// The task variable the ENGINE writes when it creates an approved
/// external task (E5-2): the plain legacy string `"true"`.
///
/// This is the same key the E3-3 worker-side gate reads
/// (`bpm_engine_worker_sdk::tools::APPROVAL_VARIABLE`); from E5-2 on, for
/// approval-required task types, the value at task delivery is
/// engine-written truth — a client-supplied value of the same name on the
/// start/user-task variable channel is overwritten by the engine on the
/// approval-required path (engine truth wins; non-gated task types keep
/// the E3-3 worker-side convention unchanged).
pub const APPROVAL_VARIABLE: &str = "__approval";

/// A persisted approval requirement for one token's visit of one
/// approval-required node (E5-2, ADR-008).
///
/// Records are keyed by `id` (a UUID the decide endpoint uses) and are
/// subject-unique on `(instance_id, node_id, token_id)`: a re-arrival of the
/// same token at the same node reuses the existing record instead of
/// creating a second approval requirement.
///
/// # Example
///
/// ```
/// use bpm_engine_core::{ApprovalEffect, ApprovalRecord, ApprovalStatus};
///
/// let record = ApprovalRecord {
///     id: "appr-1".into(),
///     instance_id: "inst-1".into(),
///     node_id: "charge".into(),
///     token_id: "token-1".into(),
///     task_type: "payment_charge".into(),
///     status: ApprovalStatus::Pending,
///     effect: ApprovalEffect::Pending,
///     decided_by: None,
///     decided_at: None,
///     note: None,
///     created_at: "1234".into(),
/// };
///
/// // Pending records have not been decided by anyone yet.
/// assert!(record.decided_by.is_none());
/// assert_eq!(record.status.as_str(), "Pending");
/// assert_eq!(record.effect, ApprovalEffect::Pending);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApprovalRecord {
    /// Unique approval record identifier (UUID).
    pub id: String,
    /// The process instance whose step requires approval.
    pub instance_id: String,
    /// The approval-required node.
    pub node_id: String,
    /// The token waiting at the node (the subject of the approval).
    pub token_id: String,
    /// The external task type of the node (what approval unlocks).
    pub task_type: String,
    /// Current lifecycle state.
    pub status: ApprovalStatus,
    /// Whether the DECISION's effect has been applied (see
    /// [`ApprovalEffect`]): `decide` leaves it `Pending`, the effect
    /// application flips it to `Applied`. Internal recovery state — not
    /// part of the REST wire contract.
    pub effect: ApprovalEffect,
    /// Actor that decided the record (the E5-1 verified `sub`), once decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    /// ISO/epoch timestamp of the decision, once decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    /// Operator note recorded with the decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Creation timestamp.
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_status_roundtrip() {
        for status in [
            ApprovalStatus::Pending,
            ApprovalStatus::Approved,
            ApprovalStatus::Rejected,
        ] {
            assert_eq!(ApprovalStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(ApprovalStatus::parse("approved"), None);
        assert_eq!(ApprovalStatus::parse(""), None);
    }

    #[test]
    fn approval_effect_roundtrip() {
        for effect in [ApprovalEffect::Pending, ApprovalEffect::Applied] {
            assert_eq!(ApprovalEffect::parse(effect.as_str()), Some(effect));
        }
        assert_eq!(ApprovalEffect::parse("applied"), None);
        assert_eq!(ApprovalEffect::parse(""), None);
    }

    #[test]
    fn approval_decision_roundtrip_and_resulting_status() {
        for decision in [ApprovalDecision::Approve, ApprovalDecision::Reject] {
            assert_eq!(ApprovalDecision::parse(decision.as_str()), Some(decision));
        }
        assert_eq!(ApprovalDecision::parse("maybe"), None);
        assert_eq!(
            ApprovalDecision::Approve.resulting_status(),
            ApprovalStatus::Approved
        );
        assert_eq!(
            ApprovalDecision::Reject.resulting_status(),
            ApprovalStatus::Rejected
        );
    }

    #[test]
    fn approval_decision_serde_wire_form() {
        assert_eq!(
            serde_json::to_string(&ApprovalDecision::Approve).unwrap(),
            "\"approve\""
        );
        assert_eq!(
            serde_json::to_string(&ApprovalDecision::Reject).unwrap(),
            "\"reject\""
        );
        assert_eq!(
            serde_json::to_string(&ApprovalStatus::Pending).unwrap(),
            "\"PENDING\""
        );
        // Backward-shaped parse: the storage strings round-trip through the
        // wire form of the other direction too.
        let parsed: ApprovalDecision = serde_json::from_str("\"reject\"").unwrap();
        assert_eq!(parsed, ApprovalDecision::Reject);
    }
}
