pub mod error;
pub mod event;
pub mod external_task;
pub mod instance;
pub mod node;
pub mod process;
pub mod saga;
pub mod token;

pub use error::*;
pub use event::*;
pub use external_task::*;
pub use instance::*;
pub use node::*;
pub use process::{EdgeCondition, Node, NodeType, OutgoingEdge, ProcessDefinition};
pub use saga::*;
pub use token::*;

// ---------------------------------------------------------------------------
// Token state machine
// ---------------------------------------------------------------------------

/// Validates whether a token status transition is legal.
///
/// State diagram (simplified view):
/// ```text
///   Created ──► Ready ──► Executing ──► Completed
///                    │           │
///                    │           ├──► Waiting ──► Ready
///                    │           │
///                    │           ├──► Suspended ──► Ready
///                    │           │
///                    │           └──► Terminated
/// ```
///
/// Terminal states (no outgoing transitions): `Completed`, `Terminated`.
pub fn is_valid_token_transition(from: TokenStatus, to: TokenStatus) -> bool {
    use TokenStatus::*;
    match (from, to) {
        // Normal startup flow
        (Created, Ready) => true,
        (Ready, Executing) => true,

        // Waiting resume
        (Waiting, Ready) => true,

        // Execution → normal completion
        (Executing, Completed) => true,

        // Execution → termination
        (Executing, Terminated) => true,

        // Execution → suspension → resume
        (Executing, Suspended) => true,
        (Suspended, Ready) => true,

        // Execution → waiting (e.g., at a gateway or timer)
        (Executing, Waiting) => true,

        // Same-state transitions are always valid (no-op)
        (s, t) if s == t => true,

        // All other transitions are illegal:
        // - Completed → anything (terminal)
        // - Terminated → anything (terminal)
        // - Created → Executing (must go through Ready)
        // - Waiting → Executing (must go through Ready)
        // - Suspended → Executing (must go through Ready)
        // - Ready → Waiting / Suspended / Completed / Terminated
        // - Created → Waiting / Suspended / Completed / Terminated
        _ => false,
    }
}

/// Returns a human-readable explanation of why a transition is valid or invalid.
pub fn transition_reason(from: TokenStatus, to: TokenStatus) -> &'static str {
    if is_valid_token_transition(from, to) {
        "valid transition"
    } else {
        match (from, to) {
            (TokenStatus::Completed, _) | (TokenStatus::Terminated, _) => {
                "terminal state has no outgoing transitions"
            }
            (TokenStatus::Created, TokenStatus::Executing) => {
                "must transition through Ready before Executing"
            }
            (TokenStatus::Waiting, TokenStatus::Executing)
            | (TokenStatus::Suspended, TokenStatus::Executing) => {
                "must transition through Ready before Executing"
            }
            (TokenStatus::Ready, TokenStatus::Waiting)
            | (TokenStatus::Ready, TokenStatus::Suspended) => {
                "Ready tokens must start executing first"
            }
            (TokenStatus::Created, TokenStatus::Waiting)
            | (TokenStatus::Created, TokenStatus::Suspended) => "invalid transition from Created",
            _ => "unknown invalid transition",
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Valid transitions — normal flow
    // -------------------------------------------------------------------------

    #[test]
    fn token_created_to_ready() {
        assert!(
            is_valid_token_transition(TokenStatus::Created, TokenStatus::Ready),
            "Created → Ready should be valid"
        );
    }

    #[test]
    fn token_ready_to_executing() {
        assert!(
            is_valid_token_transition(TokenStatus::Ready, TokenStatus::Executing),
            "Ready → Executing should be valid"
        );
    }

    #[test]
    fn token_executing_to_completed() {
        assert!(
            is_valid_token_transition(TokenStatus::Executing, TokenStatus::Completed),
            "Executing → Completed should be valid"
        );
    }

    #[test]
    fn token_executing_to_terminated() {
        assert!(
            is_valid_token_transition(TokenStatus::Executing, TokenStatus::Terminated),
            "Executing → Terminated should be valid"
        );
    }

    #[test]
    fn token_executing_to_suspended() {
        assert!(
            is_valid_token_transition(TokenStatus::Executing, TokenStatus::Suspended),
            "Executing → Suspended should be valid"
        );
    }

    #[test]
    fn token_suspended_to_ready() {
        assert!(
            is_valid_token_transition(TokenStatus::Suspended, TokenStatus::Ready),
            "Suspended → Ready should be valid (resume)"
        );
    }

    #[test]
    fn token_executing_to_waiting() {
        assert!(
            is_valid_token_transition(TokenStatus::Executing, TokenStatus::Waiting),
            "Executing → Waiting should be valid"
        );
    }

    #[test]
    fn token_waiting_to_ready() {
        assert!(
            is_valid_token_transition(TokenStatus::Waiting, TokenStatus::Ready),
            "Waiting → Ready should be valid (gateway/timer fires)"
        );
    }

    // -------------------------------------------------------------------------
    // Same-state transitions (no-op)
    // -------------------------------------------------------------------------

    #[test]
    fn token_same_state_transition_is_valid() {
        for status in [
            TokenStatus::Created,
            TokenStatus::Ready,
            TokenStatus::Executing,
            TokenStatus::Waiting,
            TokenStatus::Suspended,
            TokenStatus::Completed,
            TokenStatus::Terminated,
        ] {
            assert!(
                is_valid_token_transition(status, status),
                "{:?} → {:?} (same state) should be valid",
                status,
                status
            );
        }
    }

    // -------------------------------------------------------------------------
    // Invalid transitions — terminal states
    // -------------------------------------------------------------------------

    #[test]
    fn token_completed_to_executing_is_invalid_example() {
        // Simulates the task description's "Completed → Running" example.
        // "Running" maps to Executing in the actual enum.
        assert!(
            !is_valid_token_transition(TokenStatus::Completed, TokenStatus::Executing),
            "Completed → Executing should be invalid"
        );
    }

    #[test]
    fn token_completed_to_ready_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Completed, TokenStatus::Ready),
            "Completed → Ready should be invalid"
        );
    }

    #[test]
    fn token_completed_to_executing_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Completed, TokenStatus::Executing),
            "Completed → Executing should be invalid"
        );
    }

    #[test]
    fn token_terminated_to_any_is_invalid() {
        for to in [
            TokenStatus::Created,
            TokenStatus::Ready,
            TokenStatus::Executing,
            TokenStatus::Waiting,
            TokenStatus::Suspended,
            TokenStatus::Completed,
        ] {
            assert!(
                !is_valid_token_transition(TokenStatus::Terminated, to),
                "Terminated → {:?} should be invalid",
                to
            );
        }
    }

    // -------------------------------------------------------------------------
    // Invalid transitions — skipping states
    // -------------------------------------------------------------------------

    #[test]
    fn token_created_to_executing_is_invalid() {
        // Must go: Created → Ready → Executing
        assert!(
            !is_valid_token_transition(TokenStatus::Created, TokenStatus::Executing),
            "Created → Executing should be invalid (must go through Ready)"
        );
    }

    #[test]
    fn token_created_to_waiting_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Created, TokenStatus::Waiting),
            "Created → Waiting should be invalid"
        );
    }

    #[test]
    fn token_created_to_suspended_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Created, TokenStatus::Suspended),
            "Created → Suspended should be invalid"
        );
    }

    #[test]
    fn token_ready_to_waiting_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Ready, TokenStatus::Waiting),
            "Ready → Waiting should be invalid (must execute first)"
        );
    }

    #[test]
    fn token_ready_to_suspended_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Ready, TokenStatus::Suspended),
            "Ready → Suspended should be invalid (must execute first)"
        );
    }

    #[test]
    fn token_ready_to_completed_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Ready, TokenStatus::Completed),
            "Ready → Completed should be invalid (must execute)"
        );
    }

    #[test]
    fn token_ready_to_terminated_is_invalid() {
        assert!(
            !is_valid_token_transition(TokenStatus::Ready, TokenStatus::Terminated),
            "Ready → Terminated should be invalid (must execute)"
        );
    }

    #[test]
    fn token_waiting_to_executing_is_invalid() {
        // Must go: Waiting → Ready → Executing
        assert!(
            !is_valid_token_transition(TokenStatus::Waiting, TokenStatus::Executing),
            "Waiting → Executing should be invalid (must go through Ready)"
        );
    }

    #[test]
    fn token_suspended_to_executing_is_invalid() {
        // Must go: Suspended → Ready → Executing
        assert!(
            !is_valid_token_transition(TokenStatus::Suspended, TokenStatus::Executing),
            "Suspended → Executing should be invalid (must go through Ready)"
        );
    }

    // -------------------------------------------------------------------------
    // transition_reason
    // -------------------------------------------------------------------------

    #[test]
    fn transition_reason_describes_valid() {
        assert_eq!(
            transition_reason(TokenStatus::Created, TokenStatus::Ready),
            "valid transition"
        );
    }

    #[test]
    fn transition_reason_describes_terminal() {
        assert_eq!(
            transition_reason(TokenStatus::Completed, TokenStatus::Ready),
            "terminal state has no outgoing transitions"
        );
    }

    #[test]
    fn transition_reason_describes_skip() {
        assert_eq!(
            transition_reason(TokenStatus::Created, TokenStatus::Executing),
            "must transition through Ready before Executing"
        );
    }

    // -------------------------------------------------------------------------
    // Saga compensation ordering
    // -------------------------------------------------------------------------

    /// CompensationRecord mimics the domain type used by SagaCoordinator.
    /// The key invariant: compensation must happen in REVERSE order of execution.
    #[derive(Debug, Clone)]
    struct TestCompensationRecord {
        node_id: String,
        order: u32,
        status: &'static str,
    }

    /// Filters to only Pending records, then sorts in reverse order.
    /// This mirrors SagaCoordinator's logic:
    /// `ordered.sort_by(|a, b| b.order.cmp(&a.order))`
    fn filter_pending_and_sort_reverse(
        records: &[TestCompensationRecord],
    ) -> Vec<&TestCompensationRecord> {
        let mut pending: Vec<_> = records.iter().filter(|r| r.status == "Pending").collect();
        pending.sort_by(|a, b| b.order.cmp(&a.order));
        pending
    }

    #[test]
    fn saga_compensation_order_is_reversed() {
        // A(1) → B(2) → C(3) — all Pending
        let records = vec![
            TestCompensationRecord {
                node_id: "A".into(),
                order: 1,
                status: "Pending",
            },
            TestCompensationRecord {
                node_id: "B".into(),
                order: 2,
                status: "Pending",
            },
            TestCompensationRecord {
                node_id: "C".into(),
                order: 3,
                status: "Pending",
            },
        ];

        let result = filter_pending_and_sort_reverse(&records);
        let node_ids: Vec<_> = result.iter().map(|r| r.node_id.as_str()).collect();

        // Reverse order: C(3) → B(2) → A(1)
        assert_eq!(
            node_ids,
            &["C", "B", "A"],
            "compensation should process in reverse order: C(3) → B(2) → A(1)"
        );
    }

    #[test]
    fn saga_only_pending_are_compensated() {
        // A(1)=Completed, B(2)=Pending, C(3)=Pending
        let records = vec![
            TestCompensationRecord {
                node_id: "A".into(),
                order: 1,
                status: "Completed",
            },
            TestCompensationRecord {
                node_id: "B".into(),
                order: 2,
                status: "Pending",
            },
            TestCompensationRecord {
                node_id: "C".into(),
                order: 3,
                status: "Pending",
            },
        ];

        let result = filter_pending_and_sort_reverse(&records);
        let node_ids: Vec<_> = result.iter().map(|r| r.node_id.as_str()).collect();

        // Only Pending: C(3) → B(2)
        assert_eq!(node_ids, &["C", "B"]);
        assert!(
            !node_ids.contains(&"A"),
            "Completed records should not be compensated"
        );
    }

    #[test]
    fn saga_empty_when_no_pending() {
        // All Completed — no compensation
        let records = vec![
            TestCompensationRecord {
                node_id: "A".into(),
                order: 1,
                status: "Completed",
            },
            TestCompensationRecord {
                node_id: "B".into(),
                order: 2,
                status: "Completed",
            },
        ];

        let result = filter_pending_and_sort_reverse(&records);
        assert!(
            result.is_empty(),
            "no compensation when all records are Completed"
        );
    }

    #[test]
    fn saga_reverse_order_with_gaps() {
        // A(1), B(5), C(10) — orders have gaps
        let records = vec![
            TestCompensationRecord {
                node_id: "A".into(),
                order: 1,
                status: "Pending",
            },
            TestCompensationRecord {
                node_id: "B".into(),
                order: 5,
                status: "Pending",
            },
            TestCompensationRecord {
                node_id: "C".into(),
                order: 10,
                status: "Pending",
            },
        ];

        let result = filter_pending_and_sort_reverse(&records);
        let node_ids: Vec<_> = result.iter().map(|r| r.node_id.as_str()).collect();

        // Highest order first: C(10) → B(5) → A(1)
        assert_eq!(node_ids, &["C", "B", "A"]);
    }

    #[test]
    fn saga_single_record() {
        // Only one record
        let records = vec![TestCompensationRecord {
            node_id: "A".into(),
            order: 1,
            status: "Pending",
        }];

        let result = filter_pending_and_sort_reverse(&records);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].node_id, "A");
    }
}
