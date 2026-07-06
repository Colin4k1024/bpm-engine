//! Core domain types for the BPM engine.
//!
//! This crate is **pure logic** — no I/O, no async, no storage traits.
//! It defines the fundamental abstractions: tokens, events, process definitions,
//! external tasks, and the token state machine.

#![warn(missing_docs)]

/// Core error types for the BPM engine.
pub mod error;
/// Immutable events driving all state transitions.
pub mod event;
/// External task domain types for the worker protocol.
pub mod external_task;
/// Process instance runtime container.
pub mod instance;
/// BPMN node types and process graph structure.
pub mod node;
/// Process definition and BPMN node model.
pub mod process;
/// Saga compensation tracking and ordering.
pub mod saga;
/// Token state machine and lifecycle.
pub mod token;
/// Process variable helpers.
pub mod variable;

pub use error::*;
pub use event::*;
pub use external_task::*;
pub use instance::*;
pub use node::*;
pub use process::{
    BoundaryEventDef, EdgeCondition, FormField, FormFieldType, Node, NodeType, OutgoingEdge,
    ProcessDefinition,
};
pub use saga::*;
pub use token::*;
pub use variable::*;

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
///
/// # Example
///
/// ```
/// use bpm_engine_core::{is_valid_token_transition, TokenStatus};
///
/// // Valid transitions
/// assert!(is_valid_token_transition(TokenStatus::Created, TokenStatus::Ready));
/// assert!(is_valid_token_transition(TokenStatus::Ready, TokenStatus::Executing));
/// assert!(is_valid_token_transition(TokenStatus::Executing, TokenStatus::Waiting));
/// assert!(is_valid_token_transition(TokenStatus::Waiting, TokenStatus::Ready));
///
/// // Invalid: must go through Ready
/// assert!(!is_valid_token_transition(TokenStatus::Created, TokenStatus::Executing));
///
/// // Invalid: terminal states
/// assert!(!is_valid_token_transition(TokenStatus::Completed, TokenStatus::Ready));
/// ```
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
        pending.sort_by_key(|r| std::cmp::Reverse(r.order));
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

// ---------------------------------------------------------------------------
// Property-based tests (proptest)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // --- Token Status strategy ---

    fn token_status_strategy() -> impl Strategy<Value = TokenStatus> {
        prop_oneof![
            Just(TokenStatus::Created),
            Just(TokenStatus::Ready),
            Just(TokenStatus::Executing),
            Just(TokenStatus::Waiting),
            Just(TokenStatus::Suspended),
            Just(TokenStatus::Completed),
            Just(TokenStatus::Terminated),
        ]
    }

    // --- Property: all known valid transitions match is_valid_token_transition ---

    proptest! {
        /// Any (from, to) pair that the state machine says is valid
        /// must be one of the explicitly documented valid transitions.
        #[test]
        fn valid_transitions_are_documented(
            from in token_status_strategy(),
            to in token_status_strategy(),
        ) {
            let is_valid = is_valid_token_transition(from, to);
            let is_explicitly_valid = matches!(
                (from, to),
                (TokenStatus::Created, TokenStatus::Ready)
                    | (TokenStatus::Ready, TokenStatus::Executing)
                    | (TokenStatus::Waiting, TokenStatus::Ready)
                    | (TokenStatus::Executing, TokenStatus::Completed)
                    | (TokenStatus::Executing, TokenStatus::Terminated)
                    | (TokenStatus::Executing, TokenStatus::Suspended)
                    | (TokenStatus::Suspended, TokenStatus::Ready)
                    | (TokenStatus::Executing, TokenStatus::Waiting)
            );
            let is_same_state = from == to;

            // If the transition is valid, it must be either explicitly listed or same-state
            if is_valid {
                prop_assert!(
                    is_explicitly_valid || is_same_state,
                    "valid transition ({:?} -> {:?}) should be explicitly documented or same-state",
                    from, to
                );
            }
        }
    }

    proptest! {
        /// Terminal states (Completed, Terminated) must have NO valid outgoing transitions
        /// (except same-state no-ops).
        #[test]
        fn terminal_states_have_no_outgoing(
            to in token_status_strategy(),
            terminal in prop_oneof![Just(TokenStatus::Completed), Just(TokenStatus::Terminated)],
        ) {
            if terminal != to {
                prop_assert!(
                    !is_valid_token_transition(terminal, to),
                    "terminal state {:?} should not transition to {:?}",
                    terminal, to
                );
            }
        }
    }

    proptest! {
        /// Same-state transitions are always valid (no-op).
        #[test]
        fn same_state_is_always_valid(status in token_status_strategy()) {
            prop_assert!(
                is_valid_token_transition(status, status),
                "{:?} -> {:?} (same state) should always be valid",
                status, status
            );
        }
    }

    proptest! {
        /// transition_reason must return a non-empty string for any (from, to) pair.
        #[test]
        fn transition_reason_always_nonempty(
            from in token_status_strategy(),
            to in token_status_strategy(),
        ) {
            let reason = transition_reason(from, to);
            prop_assert!(
                !reason.is_empty(),
                "transition_reason should never be empty"
            );
        }
    }

    proptest! {
        /// transition_reason must be consistent with is_valid_token_transition.
        #[test]
        fn transition_reason_consistent_with_validity(
            from in token_status_strategy(),
            to in token_status_strategy(),
        ) {
            let is_valid = is_valid_token_transition(from, to);
            let reason = transition_reason(from, to);
            if is_valid {
                prop_assert_eq!(reason, "valid transition");
            } else {
                prop_assert_ne!(reason, "valid transition");
            }
        }
    }

    // --- Saga compensation ordering properties ---

    proptest! {
        /// For any list of (order, status) pairs, the filtered-and-sorted result:
        /// 1. Only contains Pending records
        /// 2. Is sorted in descending order
        #[test]
        fn saga_compensation_order_is_always_descending(
            records in prop::collection::vec(
                (0u32..1000, prop::bool::ANY),
                0..50,
            ),
        ) {
            #[derive(Debug, Clone)]
            struct Rec {
                order: u32,
                status: &'static str,
            }

            let recs: Vec<Rec> = records
                .into_iter()
                .map(|(order, is_pending)| Rec {
                    order,
                    status: if is_pending { "Pending" } else { "Completed" },
                })
                .collect();

            let mut pending: Vec<&Rec> = recs.iter().filter(|r| r.status == "Pending").collect();
            pending.sort_by_key(|r| std::cmp::Reverse(r.order));

            // Property 1: only Pending records remain
            for r in &pending {
                prop_assert_eq!(r.status, "Pending");
            }

            // Property 2: orders are in descending order
            for window in pending.windows(2) {
                prop_assert!(
                    window[0].order >= window[1].order,
                    "compensation orders should be descending: {} >= {}",
                    window[0].order,
                    window[1].order
                );
            }

            // Property 3: result length <= input length
            prop_assert!(pending.len() <= recs.len());
        }
    }

    proptest! {
        /// The compensation result length equals the number of Pending records.
        #[test]
        fn saga_result_length_equals_pending_count(
            records in prop::collection::vec(
                (0u32..1000, prop::bool::ANY),
                0..50,
            ),
        ) {
            let statuses: Vec<&str> = records
                .iter()
                .map(|(_, is_pending)| if *is_pending { "Pending" } else { "Completed" })
                .collect();
            let pending_count = statuses.iter().filter(|&&s| s == "Pending").count();

            #[derive(Debug)]
            struct Rec { order: u32, #[allow(dead_code)] status: &'static str }

            let recs: Vec<Rec> = records
                .into_iter()
                .zip(statuses.iter())
                .map(|((order, _), &status)| Rec { order, status })
                .collect();

            let mut result: Vec<&Rec> = recs.iter().filter(|r| r.status == "Pending").collect();
            result.sort_by_key(|r| std::cmp::Reverse(r.order));

            prop_assert_eq!(result.len(), pending_count);
        }
    }

    proptest! {
        /// For any non-empty list of Pending records with distinct orders,
        /// the first element of the sorted result has the highest order.
        #[test]
        fn saga_highest_order_is_first(
            orders in prop::collection::hash_set(0u32..10000, 1..30),
        ) {
            #[derive(Debug)]
            struct Rec { order: u32, #[allow(dead_code)] status: &'static str }

            let recs: Vec<Rec> = orders
                .into_iter()
                .map(|order| Rec { order, status: "Pending" })
                .collect();

            let mut result: Vec<&Rec> = recs.iter().collect();
            result.sort_by_key(|r| std::cmp::Reverse(r.order));

            let max_order = recs.iter().map(|r| r.order).max().unwrap();
            prop_assert_eq!(result[0].order, max_order);
        }
    }
}
