//! Invariant violation kinds for diagnosable errors (logs, history, REST).
//! See docs/invariants.md.

use std::fmt;
use thiserror::Error;

/// Kind of invariant violation, for logs and error responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantViolationKind {
    /// Token reached a final state (Completed/Terminated) more than once.
    TokenFinalizedTwice,
    /// Parallel join completed before all branches arrived.
    JoinIncomplete,
    /// External task complete/fail by a worker that does not hold the lock.
    ExternalTaskLeaseConflict,
    /// Token state transition not allowed (e.g. Completed -> Executing).
    TokenInvalidTransition,
}

impl fmt::Display for InvariantViolationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvariantViolationKind::TokenFinalizedTwice => write!(f, "TokenFinalizedTwice"),
            InvariantViolationKind::JoinIncomplete => write!(f, "JoinIncomplete"),
            InvariantViolationKind::ExternalTaskLeaseConflict => {
                write!(f, "ExternalTaskLeaseConflict")
            }
            InvariantViolationKind::TokenInvalidTransition => write!(f, "TokenInvalidTransition"),
        }
    }
}

/// Error carrying an invariant violation kind for diagnosability.
#[derive(Debug, Error)]
#[error("invariant violation: {kind} ({context})")]
pub struct InvariantViolation {
    /// The kind of invariant violation.
    pub kind: InvariantViolationKind,
    /// Human-readable context describing where the violation occurred.
    pub context: String,
}

impl InvariantViolation {
    /// Create a new invariant violation with the given kind and context.
    pub fn new(kind: InvariantViolationKind, context: impl Into<String>) -> Self {
        Self {
            kind,
            context: context.into(),
        }
    }
}
