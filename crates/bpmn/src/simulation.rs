//! Side-effect-free dry-run simulator for a compiled
//! [`ProcessDefinition`] (E2-2, issue #12).
//!
//! [`dry_run`] walks the definition from its start event with a set of
//! hypothetical start variables and reports the path it would take. It is a
//! **pure function**: no I/O, no stores, no instances, tasks, timers or
//! history are created. It never deploys anything — it is the pre-deploy
//! safety gate for AI-agent-generated definitions.
//!
//! Semantics and honest limitations (see `docs/validation-and-dry-run.md`):
//!
//! * **External tasks / service tasks / call activities / throw events are
//!   assumed to complete.** The step is recorded (`detail` carries the task
//!   type / called key / message name) and the walk continues; failures,
//!   retries and lease behavior are out of scope.
//! * **User tasks, timer / message / signal catch events park the walk**:
//!   the step is recorded and the terminus becomes [`Terminus::WaitingAt`].
//! * **Exclusive gateways** evaluate edge conditions in order, mirroring the
//!   runtime's first-match semantics: [`EdgeCondition::VariableEq`] is an
//!   exact match, [`EdgeCondition::Expression`] goes through the lightweight
//!   evaluator ([`crate::condition_lite`]) which covers `lhs == "lit"` /
//!   `lhs != "lit"` and bare-variable truthiness only; an unsupported
//!   expression is reported as unresolved ([`Terminus::UnresolvedBranch`])
//!   instead of being guessed (at runtime the full EL evaluator would handle
//!   it, treating a parse error as `false`). All-false with no default and no
//!   unconditional edge → [`Terminus::StuckAt`].
//! * **Parallel forks explore every branch**; steps from concurrent branches
//!   are interleaved in breadth-first order (a report, not a schedule). A
//!   parallel join records one step per arriving branch and continues once
//!   all `expected` branches have arrived.
//! * **Boundary events are not explored** (they are exceptional paths fired
//!   by the runtime, not sequence-flow successors).
//! * **The walk is bounded** by `max_steps` (default
//!   [`DEFAULT_MAX_STEPS`]); unbounded loops terminate with
//!   [`Terminus::MaxStepsExceeded`].

use bpm_engine_core::{EdgeCondition, NodeId, NodeType, ProcessDefinition};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};

use crate::condition_lite;

/// Default step budget for a dry-run walk.
pub const DEFAULT_MAX_STEPS: usize = 100;

/// Dry-run input: the hypothetical start variables for the walk.
#[derive(Debug, Clone, Default)]
pub struct DryRunRequest {
    /// Variable values the walk starts with (same shape as instance start
    /// variables).
    pub variables: HashMap<String, String>,
}

impl DryRunRequest {
    /// Build a request from a variable map.
    pub fn new(variables: HashMap<String, String>) -> Self {
        Self { variables }
    }

    /// Run the dry-run with this request's variables.
    pub fn run(&self, def: &ProcessDefinition, max_steps: usize) -> DryRunReport {
        dry_run(def, &self.variables, max_steps)
    }
}

/// One visited node on the dry-run walk.
#[derive(Debug, Clone, Serialize)]
pub struct DryRunStep {
    /// Node id as in the definition.
    pub node_id: String,
    /// Stable node-type name in camelCase, mirroring the engine's
    /// `NodeType` variants (`"start"`, `"externalTask"`,
    /// `"exclusiveGateway"`, `"parallelFork"`, `"parallelJoin"`, ...).
    pub node_type: String,
    /// Node payload: task type for external tasks, form key for user tasks,
    /// timer definition for timer catches, called key for call activities,
    /// message/signal name for message/signal events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// How the dry-run walk ended.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Terminus {
    /// An end event was reached; the walk completed.
    CompletedAt {
        /// The end node that completed the walk (in parallel flows, the last
        /// end event reached by any branch).
        node_id: String,
    },
    /// Execution would pause at this node (user task, timer/message/signal
    /// catch, or a parallel join waiting for remaining branches).
    WaitingAt {
        /// The node the token waits at.
        node_id: String,
        /// Why execution pauses there.
        reason: String,
    },
    /// The walk cannot continue: an exclusive gateway had every condition
    /// false and neither a default nor an unconditional edge.
    StuckAt {
        /// The node the token is stuck at.
        node_id: String,
        /// Why the token cannot move on.
        reason: String,
    },
    /// An exclusive gateway condition could not be evaluated by the
    /// lightweight evaluator; the branch is reported as unresolved instead
    /// of guessed.
    UnresolvedBranch {
        /// The gateway whose routing is undecidable.
        node_id: String,
    },
    /// The step budget was exhausted (typical for loops).
    MaxStepsExceeded {
        /// The node the walk was about to visit when the budget ran out.
        node_id: String,
    },
}

/// Full dry-run result: the walked steps and how the walk ended.
#[derive(Debug, Clone, Serialize)]
pub struct DryRunReport {
    /// Nodes visited by the walk, in visiting order (parallel branches
    /// interleave).
    pub steps: Vec<DryRunStep>,
    /// How the walk ended.
    pub terminus: Terminus,
    /// Condition expressions the lightweight evaluator could not decide
    /// (advisory; also non-empty when a default branch nevertheless
    /// resolved the routing).
    pub unresolved_conditions: Vec<String>,
    /// The step budget that bounded this walk.
    pub max_steps: usize,
}

/// Run a dry-run walk of `def` starting with `variables`, bounded by
/// `max_steps` recorded steps.
///
/// Pure function: no persistence, no engine events, no side effects.
pub fn dry_run(
    def: &ProcessDefinition,
    variables: &HashMap<String, String>,
    max_steps: usize,
) -> DryRunReport {
    let mut steps: Vec<DryRunStep> = Vec::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    let mut unresolved_conditions: Vec<String> = Vec::new();
    // Waiting events (parked tokens and starved joins) in occurrence order.
    let mut waiting_events: Vec<(String, String)> = Vec::new();
    let mut join_arrivals: HashMap<NodeId, usize> = HashMap::new();
    let mut join_first_arrival: Vec<NodeId> = Vec::new();
    let mut terminus: Option<Terminus> = None;
    let mut last_end: Option<String> = None;
    let mut last_visited: Option<String> = None;

    if def.nodes.contains_key(def.start) {
        queue.push_back(def.start);
    } else {
        terminus = Some(Terminus::StuckAt {
            node_id: def.start.to_string(),
            reason: "start node not found in definition".to_string(),
        });
    }

    while terminus.is_none() {
        let Some(nid) = queue.pop_front() else {
            break;
        };
        if steps.len() >= max_steps {
            terminus = Some(Terminus::MaxStepsExceeded {
                node_id: nid.to_string(),
            });
            break;
        }
        let Some(node) = def.nodes.get(nid) else {
            terminus = Some(Terminus::StuckAt {
                node_id: nid.to_string(),
                reason: "node not found in definition".to_string(),
            });
            break;
        };
        last_visited = Some(nid.to_string());
        steps.push(DryRunStep {
            node_id: nid.to_string(),
            node_type: node_type_name(&node.node_type).to_string(),
            detail: node_detail(&node.node_type),
        });

        match &node.node_type {
            NodeType::End | NodeType::TerminateEnd => {
                // This path completes; other queued paths keep walking.
                last_end = Some(nid.to_string());
            }
            NodeType::UserTask { form_key, .. } => {
                waiting_events.push((
                    nid.to_string(),
                    match form_key {
                        Some(key) => format!("waiting for user task completion (form: {})", key),
                        None => "waiting for user task completion".to_string(),
                    },
                ));
            }
            NodeType::TimerIntermediateCatch { timer_definition } => {
                waiting_events.push((
                    nid.to_string(),
                    format!("waiting for timer: {}", timer_definition),
                ));
            }
            NodeType::MessageIntermediateCatch { message_name } => {
                waiting_events.push((
                    nid.to_string(),
                    format!("waiting for message: {}", message_name),
                ));
            }
            NodeType::SignalIntermediateCatch { signal_name } => {
                waiting_events.push((
                    nid.to_string(),
                    format!("waiting for signal: {}", signal_name),
                ));
            }
            NodeType::ParallelJoin { expected } => {
                let arrivals = join_arrivals.entry(nid).or_insert(0);
                *arrivals += 1;
                if !join_first_arrival.contains(&nid) {
                    // First arrival at this join: remember occurrence order.
                    join_first_arrival.push(nid);
                }
                if *arrivals >= *expected {
                    for edge in &node.outgoing_edges {
                        queue.push_back(edge.target);
                    }
                }
                // Otherwise the join keeps waiting for remaining branches;
                // recorded as a waiting event at drain time below.
            }
            NodeType::ExclusiveGateway => {
                terminus = route_exclusive_gateway(
                    node,
                    variables,
                    &mut queue,
                    &mut unresolved_conditions,
                );
            }
            // Start, parallel fork, external/service tasks, call activities,
            // message/signal throws: record the step and continue along
            // every outgoing edge (external work is assumed complete).
            _ => {
                for edge in &node.outgoing_edges {
                    queue.push_back(edge.target);
                }
            }
        }
    }

    // Parallel joins that received arrivals but never reached `expected`:
    // the token would wait there at runtime.
    for join in &join_first_arrival {
        let arrived = join_arrivals[join];
        if let Some(NodeType::ParallelJoin { expected }) = def.nodes.get(join).map(|n| &n.node_type)
        {
            if arrived < *expected {
                waiting_events.push((
                    join.to_string(),
                    format!(
                        "parallel join waiting for remaining branches ({} of {} arrived)",
                        arrived, expected
                    ),
                ));
            }
        }
    }

    let terminus = terminus.unwrap_or_else(|| {
        if let Some((node_id, reason)) = waiting_events.first() {
            return Terminus::WaitingAt {
                node_id: node_id.clone(),
                reason: reason.clone(),
            };
        }
        if let Some(end) = last_end {
            return Terminus::CompletedAt { node_id: end };
        }
        Terminus::StuckAt {
            node_id: last_visited.unwrap_or_else(|| def.start.to_string()),
            reason: "walk ended without reaching an end event".to_string(),
        }
    });

    DryRunReport {
        steps,
        terminus,
        unresolved_conditions,
        max_steps,
    }
}

/// Evaluate an exclusive gateway's outgoing edges (first-match, mirroring
/// the runtime's `ExclusiveGatewayEvaluator`) and enqueue the chosen target.
/// Returns a terminus when the walk must stop (stuck / unresolved).
fn route_exclusive_gateway(
    node: &bpm_engine_core::Node,
    variables: &HashMap<String, String>,
    queue: &mut VecDeque<NodeId>,
    unresolved_conditions: &mut Vec<String>,
) -> Option<Terminus> {
    let mut default_target: Option<NodeId> = None;
    let mut unresolved = 0usize;
    for edge in &node.outgoing_edges {
        match &edge.condition {
            // Unconditional edge: taken immediately (runtime returns here).
            None => {
                queue.push_back(edge.target);
                return None;
            }
            Some(EdgeCondition::Default) => {
                default_target = Some(edge.target);
            }
            Some(EdgeCondition::VariableEq { key, value }) => {
                if variables.get(key).map(String::as_str) == Some(value.as_str()) {
                    queue.push_back(edge.target);
                    return None;
                }
            }
            Some(EdgeCondition::Expression(expr)) => {
                match condition_lite::eval_str(expr, variables) {
                    Some(true) => {
                        queue.push_back(edge.target);
                        return None;
                    }
                    Some(false) => {}
                    None => {
                        unresolved_conditions.push(expr.clone());
                        unresolved += 1;
                    }
                }
            }
        }
    }
    if let Some(target) = default_target {
        // Routing resolved by the default branch; unresolved expressions
        // remain listed in the report as advisory.
        queue.push_back(target);
        return None;
    }
    if unresolved > 0 {
        return Some(Terminus::UnresolvedBranch {
            node_id: node.id.to_string(),
        });
    }
    Some(Terminus::StuckAt {
        node_id: node.id.to_string(),
        reason: "every outgoing condition is false and there is no default branch".to_string(),
    })
}

/// Stable camelCase node-type name (mirrors the `NodeType` variant names and
/// the DSL node-type vocabulary).
fn node_type_name(node_type: &NodeType) -> &'static str {
    match node_type {
        NodeType::Start => "start",
        NodeType::End => "end",
        NodeType::ServiceTask(_) => "serviceTask",
        NodeType::UserTask { .. } => "userTask",
        NodeType::ExternalTask { .. } => "externalTask",
        NodeType::ExclusiveGateway => "exclusiveGateway",
        NodeType::ParallelFork => "parallelFork",
        NodeType::ParallelJoin { .. } => "parallelJoin",
        NodeType::TimerIntermediateCatch { .. } => "timerIntermediateCatch",
        NodeType::BoundaryTimer { .. } => "boundaryTimer",
        NodeType::BoundaryError { .. } => "boundaryError",
        NodeType::CallActivity { .. } => "callActivity",
        NodeType::MessageIntermediateCatch { .. } => "messageIntermediateCatch",
        NodeType::MessageIntermediateThrow { .. } => "messageIntermediateThrow",
        NodeType::SignalIntermediateThrow { .. } => "signalIntermediateThrow",
        NodeType::SignalIntermediateCatch { .. } => "signalIntermediateCatch",
        NodeType::TerminateEnd => "terminateEnd",
    }
}

/// Node payload for the step report.
fn node_detail(node_type: &NodeType) -> Option<String> {
    match node_type {
        NodeType::ExternalTask { task_type, .. } => Some(task_type.clone()),
        NodeType::UserTask { form_key, .. } => form_key.clone(),
        NodeType::TimerIntermediateCatch { timer_definition } => Some(timer_definition.clone()),
        NodeType::CallActivity { called_process_key } => Some(called_process_key.clone()),
        NodeType::MessageIntermediateCatch { message_name }
        | NodeType::MessageIntermediateThrow { message_name } => Some(message_name.clone()),
        NodeType::SignalIntermediateCatch { signal_name }
        | NodeType::SignalIntermediateThrow { signal_name } => Some(signal_name.clone()),
        NodeType::BoundaryTimer { duration, .. } => Some(duration.clone()),
        NodeType::BoundaryError { error_code, .. } => error_code.clone(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_core::{Node, OutgoingEdge};

    type NodeSpec = (
        &'static str,
        NodeType,
        Vec<(&'static str, Option<EdgeCondition>)>,
    );

    fn make_def(start: &'static str, nodes: Vec<NodeSpec>) -> ProcessDefinition {
        let mut map: HashMap<NodeId, Node> = HashMap::new();
        for (id, node_type, edges) in nodes {
            map.insert(
                id,
                Node {
                    id,
                    node_type,
                    outgoing_edges: edges
                        .into_iter()
                        .map(|(target, condition)| OutgoingEdge { target, condition })
                        .collect(),
                },
            );
        }
        ProcessDefinition {
            id: "test-def",
            start,
            nodes: map,
            boundary_events: HashMap::new(),
        }
    }

    fn external_task(task_type: &str) -> NodeType {
        NodeType::ExternalTask {
            task_type: task_type.to_string(),
            retries: 3,
            timeout_secs: 60,
        }
    }

    fn expr(text: &str) -> Option<EdgeCondition> {
        Some(EdgeCondition::Expression(text.to_string()))
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn step_ids(report: &DryRunReport) -> Vec<&str> {
        report.steps.iter().map(|s| s.node_id.as_str()).collect()
    }

    #[test]
    fn walks_straight_line_to_end() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("work", None)]),
                ("work", external_task("orders"), vec![("end", None)]),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "work", "end"]);
        // External tasks are assumed complete: step recorded with the task
        // type, then the walk continues.
        assert_eq!(report.steps[1].node_type, "externalTask");
        assert_eq!(report.steps[1].detail.as_deref(), Some("orders"));
        assert!(
            matches!(report.terminus, Terminus::CompletedAt { ref node_id } if node_id == "end")
        );
        assert!(report.unresolved_conditions.is_empty());
    }

    #[test]
    fn routes_exclusive_gateway_by_exact_condition() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("gw", None)]),
                (
                    "gw",
                    NodeType::ExclusiveGateway,
                    vec![
                        ("task_a", expr("x == \"a\"")),
                        ("task_b", Some(EdgeCondition::Default)),
                    ],
                ),
                ("task_a", external_task("work-a"), vec![("end", None)]),
                ("task_b", external_task("work-b"), vec![("end", None)]),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &vars(&[("x", "a")]), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "task_a", "end"]);
        assert!(matches!(report.terminus, Terminus::CompletedAt { .. }));

        // Non-matching value: default branch wins.
        let report = dry_run(&def, &vars(&[("x", "zzz")]), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "task_b", "end"]);

        // No variable at all: default branch wins.
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "task_b", "end"]);
    }

    #[test]
    fn routes_by_neq_and_variable_eq() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("gw", None)]),
                (
                    "gw",
                    NodeType::ExclusiveGateway,
                    vec![
                        ("no", expr("x != \"a\"")),
                        (
                            "yes",
                            Some(EdgeCondition::VariableEq {
                                key: "x".to_string(),
                                value: "a".to_string(),
                            }),
                        ),
                    ],
                ),
                ("no", NodeType::End, vec![]),
                ("yes", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &vars(&[("x", "a")]), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "yes"]);
        let report = dry_run(&def, &vars(&[("x", "b")]), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "no"]);
    }

    #[test]
    fn unconditional_edge_is_taken_immediately() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("gw", None)]),
                (
                    "gw",
                    NodeType::ExclusiveGateway,
                    vec![("always", None), ("never", expr("x == \"a\""))],
                ),
                ("always", NodeType::End, vec![]),
                ("never", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        // Runtime first-match: the unconditional edge wins regardless of
        // the condition on the later edge.
        assert_eq!(step_ids(&report), vec!["start", "gw", "always"]);
    }

    #[test]
    fn stuck_when_all_conditions_false_without_default() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("gw", None)]),
                (
                    "gw",
                    NodeType::ExclusiveGateway,
                    vec![("a", expr("x == \"a\"")), ("b", expr("x == \"b\""))],
                ),
                ("a", NodeType::End, vec![]),
                ("b", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &vars(&[("x", "c")]), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw"]);
        assert!(matches!(
            &report.terminus,
            Terminus::StuckAt { node_id, reason } if node_id == "gw"
                && reason.contains("no default branch")
        ));
    }

    #[test]
    fn unresolved_branch_for_expressions_outside_the_subset() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("gw", None)]),
                (
                    "gw",
                    NodeType::ExclusiveGateway,
                    vec![
                        ("big", expr("amount > 100")),
                        ("small", expr("amount <= 100")),
                    ],
                ),
                ("big", NodeType::End, vec![]),
                ("small", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &vars(&[("amount", "500")]), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw"]);
        assert!(matches!(
            &report.terminus,
            Terminus::UnresolvedBranch { node_id } if node_id == "gw"
        ));
        assert!(report
            .unresolved_conditions
            .iter()
            .any(|c| c == "amount > 100"));
        assert!(report
            .unresolved_conditions
            .iter()
            .any(|c| c == "amount <= 100"));
    }

    #[test]
    fn unresolved_expressions_are_advisory_when_default_resolves() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("gw", None)]),
                (
                    "gw",
                    NodeType::ExclusiveGateway,
                    vec![
                        ("big", expr("amount > 100")),
                        ("fallback", Some(EdgeCondition::Default)),
                    ],
                ),
                ("big", NodeType::End, vec![]),
                ("fallback", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        // Default resolved the routing; the unevaluable expression is still
        // reported in unresolved_conditions.
        assert_eq!(step_ids(&report), vec!["start", "gw", "fallback"]);
        assert!(matches!(report.terminus, Terminus::CompletedAt { .. }));
        assert_eq!(
            report.unresolved_conditions,
            vec!["amount > 100".to_string()]
        );
    }

    #[test]
    fn waits_at_user_task_and_timer_catch() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("review", None)]),
                (
                    "review",
                    NodeType::UserTask {
                        form_key: Some("approval-form".to_string()),
                        form_fields: None,
                    },
                    vec![("wait", None)],
                ),
                (
                    "wait",
                    NodeType::TimerIntermediateCatch {
                        timer_definition: "PT1H".to_string(),
                    },
                    vec![("end", None)],
                ),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "review"]);
        assert!(matches!(
            &report.terminus,
            Terminus::WaitingAt { node_id, reason } if node_id == "review"
                && reason.contains("approval-form")
        ));
    }

    #[test]
    fn waits_at_timer_catch_when_reached() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("wait", None)]),
                (
                    "wait",
                    NodeType::TimerIntermediateCatch {
                        timer_definition: "PT1H".to_string(),
                    },
                    vec![("end", None)],
                ),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "wait"]);
        assert!(matches!(
            &report.terminus,
            Terminus::WaitingAt { node_id, reason } if node_id == "wait"
                && reason.contains("PT1H")
        ));
        assert_eq!(report.steps[1].node_type, "timerIntermediateCatch");
        assert_eq!(report.steps[1].detail.as_deref(), Some("PT1H"));
    }

    #[test]
    fn parallel_fork_walks_both_branches_interleaved() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("fork", None)]),
                (
                    "fork",
                    NodeType::ParallelFork,
                    vec![("a1", None), ("b1", None)],
                ),
                ("a1", external_task("work-a1"), vec![("join", None)]),
                ("b1", external_task("work-b1"), vec![("join", None)]),
                (
                    "join",
                    NodeType::ParallelJoin { expected: 2 },
                    vec![("end", None)],
                ),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        // Both branches appear, breadth-first interleaved; the join records
        // one step per arriving branch and continues after both arrive.
        assert_eq!(
            step_ids(&report),
            vec!["start", "fork", "a1", "b1", "join", "join", "end"]
        );
        assert!(
            matches!(report.terminus, Terminus::CompletedAt { ref node_id } if node_id == "end")
        );
    }

    #[test]
    fn join_waiting_for_missing_branch_is_reported_as_waiting() {
        // fork -> a1 -> join (expected 2); fork -> review (user task, parks).
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("fork", None)]),
                (
                    "fork",
                    NodeType::ParallelFork,
                    vec![("a1", None), ("review", None)],
                ),
                ("a1", external_task("work"), vec![("join", None)]),
                (
                    "join",
                    NodeType::ParallelJoin { expected: 2 },
                    vec![("end", None)],
                ),
                (
                    "review",
                    NodeType::UserTask {
                        form_key: None,
                        form_fields: None,
                    },
                    vec![("join", None)],
                ),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert!(matches!(
            &report.terminus,
            Terminus::WaitingAt { node_id, .. } if node_id == "review"
        ));
    }

    #[test]
    fn join_never_reaching_expected_arrivals_is_waiting() {
        // fork -> a1 -> join (expected 2); fork -> dead end (End event).
        // The join starves: 1 of 2 branches arrived.
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("fork", None)]),
                (
                    "fork",
                    NodeType::ParallelFork,
                    vec![("a1", None), ("stop", None)],
                ),
                ("a1", external_task("work"), vec![("join", None)]),
                (
                    "join",
                    NodeType::ParallelJoin { expected: 2 },
                    vec![("end", None)],
                ),
                ("stop", NodeType::End, vec![]),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert!(matches!(
            &report.terminus,
            Terminus::WaitingAt { node_id, reason } if node_id == "join"
                && reason.contains("1 of 2")
        ));
    }

    #[test]
    fn loops_terminate_with_max_steps_exceeded() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("loop1", None)]),
                ("loop1", external_task("x"), vec![("loop2", None)]),
                ("loop2", external_task("x"), vec![("loop1", None)]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), 5);
        assert_eq!(report.max_steps, 5);
        assert_eq!(report.steps.len(), 5);
        assert!(matches!(
            &report.terminus,
            Terminus::MaxStepsExceeded { node_id } if node_id == "loop1" || node_id == "loop2"
        ));
        // Default budget also terminates the loop.
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert!(matches!(report.terminus, Terminus::MaxStepsExceeded { .. }));
    }

    #[test]
    fn message_and_signal_catch_wait() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("catch_msg", None)]),
                (
                    "catch_msg",
                    NodeType::MessageIntermediateCatch {
                        message_name: "order-confirmed".to_string(),
                    },
                    vec![("catch_sig", None)],
                ),
                (
                    "catch_sig",
                    NodeType::SignalIntermediateCatch {
                        signal_name: "broadcast".to_string(),
                    },
                    vec![("end", None)],
                ),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "catch_msg"]);
        assert!(matches!(
            &report.terminus,
            Terminus::WaitingAt { node_id, reason } if node_id == "catch_msg"
                && reason.contains("order-confirmed")
        ));
    }

    #[test]
    fn call_activity_and_throw_events_continue_with_detail() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("call", None)]),
                (
                    "call",
                    NodeType::CallActivity {
                        called_process_key: "child-proc".to_string(),
                    },
                    vec![("emit", None)],
                ),
                (
                    "emit",
                    NodeType::MessageIntermediateThrow {
                        message_name: "notify".to_string(),
                    },
                    vec![("end", None)],
                ),
                ("end", NodeType::End, vec![]),
            ],
        );
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "call", "emit", "end"]);
        assert_eq!(report.steps[1].node_type, "callActivity");
        assert_eq!(report.steps[1].detail.as_deref(), Some("child-proc"));
        assert_eq!(report.steps[2].node_type, "messageIntermediateThrow");
        assert_eq!(report.steps[2].detail.as_deref(), Some("notify"));
        assert!(matches!(report.terminus, Terminus::CompletedAt { .. }));
    }

    #[test]
    fn missing_start_node_is_stuck() {
        let def = make_def("nope", vec![("end", NodeType::End, vec![])]);
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert!(report.steps.is_empty());
        assert!(matches!(
            &report.terminus,
            Terminus::StuckAt { node_id, reason } if node_id == "nope"
                && reason.contains("not found")
        ));
    }

    #[test]
    fn dry_run_request_runs() {
        let def = make_def(
            "start",
            vec![
                ("start", NodeType::Start, vec![("gw", None)]),
                (
                    "gw",
                    NodeType::ExclusiveGateway,
                    vec![
                        ("a", expr("x == \"a\"")),
                        ("b", Some(EdgeCondition::Default)),
                    ],
                ),
                ("a", NodeType::End, vec![]),
                ("b", NodeType::End, vec![]),
            ],
        );
        let request = DryRunRequest::new(vars(&[("x", "a")]));
        let report = request.run(&def, DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "a"]);
        // Default-constructed request is an empty variable map.
        assert!(DryRunRequest::default().variables.is_empty());
    }

    #[test]
    fn compiled_bpmn_gateway_walks_the_conditioned_path() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="routing" isExecutable="true">
    <startEvent id="start"/>
    <exclusiveGateway id="gw" default="flow_default"/>
    <serviceTask id="small">
      <extensionElements>
        <taskDefinition type="ship-standard"/>
      </extensionElements>
    </serviceTask>
    <serviceTask id="big">
      <extensionElements>
        <taskDefinition type="ship-express"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="flow1" sourceRef="start" targetRef="gw"/>
    <sequenceFlow id="flow_big" sourceRef="gw" targetRef="big">
      <conditionExpression>amount == "100"</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="flow_default" sourceRef="gw" targetRef="small"/>
    <sequenceFlow id="flow3" sourceRef="big" targetRef="end"/>
    <sequenceFlow id="flow4" sourceRef="small" targetRef="end"/>
  </process>
</definitions>"#;
        let def = crate::parse_and_compile(xml).expect("compiles");
        let report = dry_run(&def, &vars(&[("amount", "100")]), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "big", "end"]);
        assert_eq!(report.steps[2].detail.as_deref(), Some("ship-express"));
        let report = dry_run(&def, &HashMap::new(), DEFAULT_MAX_STEPS);
        assert_eq!(step_ids(&report), vec!["start", "gw", "small", "end"]);
        assert_eq!(report.steps[2].detail.as_deref(), Some("ship-standard"));
    }
}
