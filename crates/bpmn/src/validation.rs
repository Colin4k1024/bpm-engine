//! Static validation of a compiled [`ProcessDefinition`] (E2-2, issue #12).
//!
//! [`validate_definition`] runs a set of graph and configuration checks on
//! the *compiled* definition — a deeper analysis than the compiler's own
//! checks (which reject malformed documents outright during compile). It
//! never fails: every problem is returned as a [`ValidationFinding`] with a
//! stable snake_case [`ValidationFinding::code`], and an empty result means
//! the definition is clean.
//!
//! Finding codes:
//!
//! | Code | Severity | Meaning |
//! |------|----------|---------|
//! | `unreachable_node` | Critical | Node not reachable from the start event (graph reachability — edge conditions are ignored) |
//! | `missing_default_branch` | Critical | Exclusive gateway fully conditional with no default/unconditional edge (an exhaustive `==`/`!=` complement pair on the same variable is exempt) |
//! | `dead_branch_after_unconditional` | Warning | Conditional/default edge listed after an unconditional edge of its exclusive gateway: the runtime takes the first unconditional edge immediately, so the later edge can never be taken |
//! | `join_in_cycle` | Warning | Parallel join inside a cycle: joins re-arm per iteration, but a branch reaching the join without its sibling starves that iteration's join group |
//! | `cycle_detected` | Warning | Any cycle in the flow (loops are legal; agents should know) |
//! | `invalid_task_config` | Critical | External task with empty `task_type` or negative `retries` |
//! | `task_timeout_zero` | Warning | External task with `timeout_secs == 0` |
//! | `duplicate_form_field` | Warning | User task form fields with duplicate ids |
//! | `condition_variable_scope` | Info | Input contract: variables referenced by edge conditions |
//!
//! Boundary events count as reachable through their host activity (they are
//! triggered by the host, not by incoming sequence flows), so a definition
//! with boundary events is not falsely flagged as unreachable.
//!
//! Two scope notes. `unreachable_node` is a pure GRAPH property: edges are
//! followed regardless of their conditions, so a node sitting behind an
//! always-false condition still counts as reachable (the condition-aware
//! checks are `missing_default_branch` and `dead_branch_after_unconditional`).
//! `dead_branch_after_unconditional` evaluates `Node::outgoing_edges` in
//! list order, which is the source document's sequence-flow order — the
//! parser wires flows in document order and the compiler preserves it — the
//! same order the runtime's gateway evaluator consults.

use bpm_engine_core::{EdgeCondition, Node, NodeId, NodeType, OutgoingEdge, ProcessDefinition};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::condition_lite;

/// Severity of a [`ValidationFinding`]. `Critical` findings make a
/// definition invalid; `Warning`/`Info` are advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Severity {
    /// The definition cannot execute correctly as written.
    Critical,
    /// Legal but a common source of surprises; review advised.
    Warning,
    /// Informational contract report; no action required.
    Info,
}

fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Critical => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    }
}

/// A single static-validation finding with a stable machine-readable code.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationFinding {
    /// How severe the finding is.
    pub severity: Severity,
    /// Stable snake_case code (see the module docs table). Do not rename.
    pub code: &'static str,
    /// Node the finding refers to, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Human-readable description with remediation context.
    pub message: String,
}

/// Statically validate a compiled process definition.
///
/// Always returns findings (empty = clean); never fails. Findings are
/// deterministically ordered by severity, then code, then node id.
pub fn validate_definition(def: &ProcessDefinition) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    unreachable_nodes(def, &mut findings);
    missing_default_branches(def, &mut findings);
    dead_branches_after_unconditional(def, &mut findings);
    cycles_and_cycle_joins(def, &mut findings);
    task_configs(def, &mut findings);
    user_task_form_fields(def, &mut findings);
    condition_variables(def, &mut findings);
    // HashMap iteration order is randomized; sort for a stable contract.
    findings.sort_by(|a, b| {
        severity_rank(a.severity)
            .cmp(&severity_rank(b.severity))
            .then_with(|| a.code.cmp(b.code))
            .then_with(|| a.node_id.cmp(&b.node_id))
            .then_with(|| a.message.cmp(&b.message))
    });
    findings
}

// --- unreachable_node -------------------------------------------------------

/// Nodes reachable from the start event by following outgoing sequence-flow
/// edges. This is GRAPH reachability: an edge is followed regardless of its
/// condition, so a node behind an always-false condition still counts as
/// reachable (the condition-aware checks are `missing_default_branch` and
/// `dead_branch_after_unconditional`). Boundary events are reachable
/// through their host activity, and their outgoing targets are explored
/// from the boundary node.
fn reachable_nodes(def: &ProcessDefinition) -> HashSet<NodeId> {
    let mut reachable: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();
    if def.nodes.contains_key(def.start) {
        queue.push_back(def.start);
    }
    while let Some(nid) = queue.pop_front() {
        if !reachable.insert(nid) {
            continue;
        }
        if let Some(events) = def.boundary_events.get(nid) {
            for event in events {
                if def.nodes.contains_key(event.node_id) {
                    queue.push_back(event.node_id);
                }
            }
        }
        if let Some(node) = def.nodes.get(nid) {
            for edge in &node.outgoing_edges {
                if def.nodes.contains_key(edge.target) {
                    queue.push_back(edge.target);
                }
            }
        }
    }
    reachable
}

fn unreachable_nodes(def: &ProcessDefinition, findings: &mut Vec<ValidationFinding>) {
    let reachable = reachable_nodes(def);
    let mut ids: Vec<NodeId> = def
        .nodes
        .keys()
        .copied()
        .filter(|id| !reachable.contains(id))
        .collect();
    ids.sort_unstable();
    for id in ids {
        findings.push(ValidationFinding {
            severity: Severity::Critical,
            code: "unreachable_node",
            node_id: Some(id.to_string()),
            message: format!(
                "node '{}' is not reachable from the start event '{}'",
                id, def.start
            ),
        });
    }
}

// --- missing_default_branch -------------------------------------------------

fn missing_default_branches(def: &ProcessDefinition, findings: &mut Vec<ValidationFinding>) {
    for (id, node) in sorted_node_entries(def) {
        if !matches!(node.node_type, NodeType::ExclusiveGateway) {
            continue;
        }
        let mut has_conditional = false;
        let mut has_default = false;
        let mut has_unconditional = false;
        for edge in &node.outgoing_edges {
            match &edge.condition {
                Some(EdgeCondition::Default) => has_default = true,
                Some(_) => has_conditional = true,
                None => has_unconditional = true,
            }
        }
        if has_conditional
            && !has_default
            && !has_unconditional
            && !has_exhaustive_complement_pair(&node.outgoing_edges)
        {
            findings.push(ValidationFinding {
                severity: Severity::Critical,
                code: "missing_default_branch",
                node_id: Some(id.to_string()),
                message: format!(
                    "exclusive gateway '{}' has only conditional outgoing edges and neither a default nor an unconditional edge: a token gets stuck when every condition is false",
                    id
                ),
            });
        }
    }
}

/// The `==`/`!=` comparison an edge condition expresses, when the
/// lightweight subset covers it. `VariableEq` is exactly `key == "value"`
/// (the DSL lowers its `variableEq` to the same canonical expression), so
/// both condition forms feed the same structural comparison.
fn edge_comparison(condition: &EdgeCondition) -> Option<condition_lite::LiteComparison> {
    match condition {
        EdgeCondition::VariableEq { key, value } => Some(condition_lite::LiteComparison {
            lhs: key.clone(),
            operator: condition_lite::LiteOperator::Eq,
            rhs: condition_lite::LiteOperand::Quoted(value.clone()),
        }),
        EdgeCondition::Expression(expr) => condition_lite::parse(expr).comparison(),
        EdgeCondition::Default => None,
    }
}

/// Mutually exclusive and exhaustive branch pair: the same variable
/// compared against the same quoted literal with `==` on one edge and
/// `!=` on another. Exactly one of the two conditions is true for every
/// value of the variable, so the gateway can never be stuck for lack of a
/// default branch. Quoted literals only: a bare-token operand resolves as
/// a variable at runtime and is not statically a fixed literal.
fn has_exhaustive_complement_pair(edges: &[OutgoingEdge]) -> bool {
    // (lhs, literal) -> (saw ==, saw !=)
    let mut seen: HashMap<(String, String), (bool, bool)> = HashMap::new();
    for edge in edges {
        let Some(condition) = &edge.condition else {
            continue;
        };
        let Some(comparison) = edge_comparison(condition) else {
            continue;
        };
        let condition_lite::LiteOperand::Quoted(literal) = &comparison.rhs else {
            continue;
        };
        let entry = seen.entry((comparison.lhs, literal.clone())).or_default();
        match comparison.operator {
            condition_lite::LiteOperator::Eq => entry.0 = true,
            condition_lite::LiteOperator::Neq => entry.1 = true,
        }
    }
    seen.values().any(|&(eq, neq)| eq && neq)
}

// --- dead_branch_after_unconditional ----------------------------------------

/// The runtime's exclusive-gateway evaluator takes the FIRST unconditional
/// edge and returns immediately (`None => return vec![edge.target]` in
/// `crates/runtime/src/gateway.rs`, mirrored by
/// `evaluate_exclusive_gateway` in `crates/runtime/src/transition.rs`), so
/// any conditional/default edge listed after an unconditional edge is never
/// consulted. `Node::outgoing_edges` preserves the source document's
/// sequence-flow order, so "listed after" is exactly the source order.
fn dead_branches_after_unconditional(
    def: &ProcessDefinition,
    findings: &mut Vec<ValidationFinding>,
) {
    for (id, node) in sorted_node_entries(def) {
        if !matches!(node.node_type, NodeType::ExclusiveGateway) {
            continue;
        }
        let mut unconditional: Option<NodeId> = None;
        for edge in &node.outgoing_edges {
            match &edge.condition {
                None => {
                    if unconditional.is_none() {
                        unconditional = Some(edge.target);
                    }
                }
                Some(condition) => {
                    let Some(uncond) = unconditional else {
                        continue;
                    };
                    let kind = if matches!(condition, EdgeCondition::Default) {
                        "default"
                    } else {
                        "conditional"
                    };
                    findings.push(ValidationFinding {
                        severity: Severity::Warning,
                        code: "dead_branch_after_unconditional",
                        node_id: Some(edge.target.to_string()),
                        message: format!(
                            "exclusive gateway '{}' lists a {} edge to '{}' after its unconditional edge to '{}': the runtime evaluator takes the first unconditional edge immediately, so the later edge can never be taken (reorder the flows so the unconditional edge comes last)",
                            id, kind, edge.target, uncond
                        ),
                    });
                }
            }
        }
    }
}

// --- cycle_detected + join_in_cycle -----------------------------------------

/// Strongly connected components (Tarjan) over the flow graph, including the
/// host → boundary-event hops. Deterministic: roots and neighbor lists are
/// visited in sorted order.
fn strongly_connected_components(def: &ProcessDefinition) -> Vec<Vec<NodeId>> {
    let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for (id, node) in &def.nodes {
        let targets = adjacency.entry(id).or_default();
        for edge in &node.outgoing_edges {
            targets.push(edge.target);
        }
        if let Some(events) = def.boundary_events.get(id) {
            for event in events {
                targets.push(event.node_id);
            }
        }
    }

    let mut ctx = TarjanCtx {
        adjacency: &adjacency,
        index: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        on_stack: HashSet::new(),
        sccs: Vec::new(),
        next_index: 0,
    };
    let mut roots: Vec<NodeId> = def.nodes.keys().copied().collect();
    roots.sort_unstable();
    for root in roots {
        if !ctx.index.contains_key(root) {
            ctx.visit(root);
        }
    }
    ctx.sccs
}

struct TarjanCtx<'a> {
    adjacency: &'a HashMap<NodeId, Vec<NodeId>>,
    index: HashMap<NodeId, usize>,
    low: HashMap<NodeId, usize>,
    stack: Vec<NodeId>,
    on_stack: HashSet<NodeId>,
    sccs: Vec<Vec<NodeId>>,
    next_index: usize,
}

impl TarjanCtx<'_> {
    fn visit(&mut self, v: NodeId) {
        self.index.insert(v, self.next_index);
        self.low.insert(v, self.next_index);
        self.next_index += 1;
        self.stack.push(v);
        self.on_stack.insert(v);
        // Clone the neighbor list: `visit` needs &mut self.
        let neighbors = self.adjacency.get(v).cloned().unwrap_or_default();
        for w in neighbors {
            if !self.index.contains_key(w) {
                self.visit(w);
                let low_w = self.low[&w];
                let low_v = self.low[&v];
                if low_w < low_v {
                    self.low.insert(v, low_w);
                }
            } else if self.on_stack.contains(&w) {
                let index_w = self.index[&w];
                let low_v = self.low[&v];
                if index_w < low_v {
                    self.low.insert(v, index_w);
                }
            }
        }
        if self.low[&v] == self.index[&v] {
            let mut component = Vec::new();
            while let Some(w) = self.stack.pop() {
                self.on_stack.remove(&w);
                component.push(w);
                if w == v {
                    break;
                }
            }
            self.sccs.push(component);
        }
    }
}

fn cycles_and_cycle_joins(def: &ProcessDefinition, findings: &mut Vec<ValidationFinding>) {
    let mut cycles: Vec<Vec<NodeId>> = strongly_connected_components(def)
        .into_iter()
        .filter(|component| component.len() > 1)
        .collect();
    // Self-loops are rejected by the compiler (`source == target`), so every
    // cycle here is a component of at least two nodes.
    for cycle in &mut cycles {
        cycle.sort_unstable();
    }
    cycles.sort();

    for cycle in cycles {
        let members: Vec<&str> = cycle.to_vec();
        let listed = members.join(" -> ");
        findings.push(ValidationFinding {
            severity: Severity::Warning,
            code: "cycle_detected",
            node_id: Some(members[0].to_string()),
            message: format!(
                "flow contains a cycle ({}); loops are legal but every traversal is unbounded — make sure exit conditions are reachable",
                listed
            ),
        });
        for member in members {
            let Some(node) = def.nodes.get(member) else {
                continue;
            };
            if matches!(node.node_type, NodeType::ParallelJoin { .. }) {
                // The runtime re-arms joins per iteration: every fork
                // execution creates a fresh join group
                // (crates/runtime/src/token_arrived_handler.rs:
                // `Uuid::new_v4()` + `ensure_group`), each group fires
                // exactly once (`MemoryRepo::try_join`'s once-guard), and
                // the join continuation leaves the group (`move_token`
                // sets `parallel_group_id = None`). A join inside a loop is
                // therefore NOT a deadlock by itself — the honest hazard is
                // uneven arrivals within one iteration's group.
                findings.push(ValidationFinding {
                    severity: Severity::Warning,
                    code: "join_in_cycle",
                    node_id: Some(member.to_string()),
                    message: format!(
                        "parallel join '{}' is part of a cycle ({}): joins re-arm per iteration (each fork execution creates a fresh join group that fires once), so a clean fork -> branches -> join -> loop-back flow is valid — but a branch that reaches the join without its sibling leaves that iteration's group below its expected count, starving the join",
                        member, listed
                    ),
                });
            }
        }
    }
}

// --- invalid_task_config / task_timeout_zero --------------------------------

fn task_configs(def: &ProcessDefinition, findings: &mut Vec<ValidationFinding>) {
    for (id, node) in sorted_node_entries(def) {
        if let NodeType::ExternalTask {
            task_type,
            retries,
            timeout_secs,
        } = &node.node_type
        {
            if task_type.trim().is_empty() {
                findings.push(ValidationFinding {
                    severity: Severity::Critical,
                    code: "invalid_task_config",
                    node_id: Some(id.to_string()),
                    message: format!(
                        "external task '{}' has an empty task_type; workers subscribe by task type, so the task can never be fetched",
                        id
                    ),
                });
            }
            if *retries < 0 {
                findings.push(ValidationFinding {
                    severity: Severity::Critical,
                    code: "invalid_task_config",
                    node_id: Some(id.to_string()),
                    message: format!(
                        "external task '{}' has negative retries ({}); retries must be >= 0",
                        id, retries
                    ),
                });
            }
            if *timeout_secs == 0 {
                findings.push(ValidationFinding {
                    severity: Severity::Warning,
                    code: "task_timeout_zero",
                    node_id: Some(id.to_string()),
                    message: format!(
                        "external task '{}' has timeout_secs = 0; the lease expires immediately and the task is reclaimed before a worker can complete it",
                        id
                    ),
                });
            }
        }
    }
}

// --- duplicate_form_field ---------------------------------------------------

fn user_task_form_fields(def: &ProcessDefinition, findings: &mut Vec<ValidationFinding>) {
    for (id, node) in sorted_node_entries(def) {
        if let NodeType::UserTask {
            form_fields: Some(fields),
            ..
        } = &node.node_type
        {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut duplicates: BTreeSet<&str> = BTreeSet::new();
            for field in fields {
                if !seen.insert(field.id.as_str()) {
                    duplicates.insert(field.id.as_str());
                }
            }
            for duplicate in duplicates {
                findings.push(ValidationFinding {
                    severity: Severity::Warning,
                    code: "duplicate_form_field",
                    node_id: Some(id.to_string()),
                    message: format!(
                        "user task '{}' has duplicate form field id '{}'",
                        id, duplicate
                    ),
                });
            }
        }
    }
}

// --- condition_variable_scope -----------------------------------------------

/// Input contract: every variable referenced by an edge condition is
/// reported once (an Info finding), so callers know which values to provide
/// when starting an instance. Whether a variable is actually *produced*
/// (task outputs are dynamic) cannot be proven statically — this is an
/// informational contract report, not an error.
fn condition_variables(def: &ProcessDefinition, findings: &mut Vec<ValidationFinding>) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for node in def.nodes.values() {
        for edge in &node.outgoing_edges {
            match &edge.condition {
                Some(EdgeCondition::VariableEq { key, .. }) => {
                    *counts.entry(key.clone()).or_insert(0) += 1;
                }
                Some(EdgeCondition::Expression(expr)) => {
                    for variable in condition_lite::referenced_variables(expr) {
                        *counts.entry(variable).or_insert(0) += 1;
                    }
                }
                _ => {}
            }
        }
    }
    for (variable, references) in counts {
        findings.push(ValidationFinding {
            severity: Severity::Info,
            code: "condition_variable_scope",
            node_id: None,
            message: format!(
                "condition variable '{}' is referenced by {} edge condition(s); provide it at start or via a task output (informational input contract — production by task outputs cannot be proven statically)",
                variable, references
            ),
        });
    }
}

// --- helpers ----------------------------------------------------------------

fn sorted_node_entries(def: &ProcessDefinition) -> Vec<(NodeId, &Node)> {
    let mut entries: Vec<(NodeId, &Node)> =
        def.nodes.iter().map(|(id, node)| (*id, node)).collect();
    entries.sort_by_key(|(id, _)| *id);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_core::{FormField, FormFieldType, OutgoingEdge};

    type NodeSpec = (
        &'static str,
        NodeType,
        Vec<(&'static str, Option<EdgeCondition>)>,
    );

    fn make_def(nodes: Vec<NodeSpec>) -> ProcessDefinition {
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
            start: "start",
            nodes: map,
            boundary_events: HashMap::new(),
        }
    }

    fn external_task(task_type: &str, retries: i32, timeout_secs: u64) -> NodeType {
        NodeType::ExternalTask {
            task_type: task_type.to_string(),
            retries,
            timeout_secs,
        }
    }

    fn form_field(id: &str) -> FormField {
        FormField {
            id: id.to_string(),
            label: id.to_string(),
            field_type: FormFieldType::String,
            required: false,
            default_value: None,
            options: None,
        }
    }

    fn codes(findings: &[ValidationFinding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.code).collect()
    }

    fn finding<'a>(findings: &'a [ValidationFinding], code: &str) -> &'a ValidationFinding {
        findings
            .iter()
            .find(|f| f.code == code)
            .unwrap_or_else(|| panic!("finding '{}' not found in {:?}", code, findings))
    }

    #[test]
    fn clean_definition_has_no_findings() {
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("work", None)]),
            ("work", external_task("orders", 3, 60), vec![("end", None)]),
            ("end", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        assert!(
            findings.is_empty(),
            "expected no findings, got {:?}",
            findings
        );
    }

    #[test]
    fn unreachable_nodes_are_critical() {
        // start -> end plus an island a -> b -> a.
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("end", None)]),
            ("end", NodeType::End, vec![]),
            ("isle_a", external_task("x", 3, 60), vec![("isle_b", None)]),
            ("isle_b", external_task("x", 3, 60), vec![("isle_a", None)]),
        ]);
        let findings = validate_definition(&def);
        let unreachable: Vec<_> = findings
            .iter()
            .filter(|f| f.code == "unreachable_node")
            .collect();
        assert_eq!(unreachable.len(), 2);
        assert!(unreachable.iter().all(|f| f.severity == Severity::Critical));
        assert!(unreachable
            .iter()
            .any(|f| f.node_id.as_deref() == Some("isle_a")));
        assert!(unreachable
            .iter()
            .any(|f| f.node_id.as_deref() == Some("isle_b")));
        // The island is also a cycle: one Warning.
        assert_eq!(
            findings
                .iter()
                .filter(|f| f.code == "cycle_detected")
                .count(),
            1
        );
    }

    #[test]
    fn boundary_events_are_reachable_through_their_host() {
        let mut def = make_def(vec![
            ("start", NodeType::Start, vec![("host", None)]),
            ("host", external_task("work", 3, 60), vec![("end", None)]),
            ("end", NodeType::End, vec![]),
            ("timeout", NodeType::End, vec![]),
        ]);
        def.boundary_events.insert(
            "host",
            vec![bpm_engine_core::BoundaryEventDef {
                node_id: "timeout",
                is_interrupting: true,
                target_node_id: "timeout",
            }],
        );
        let findings = validate_definition(&def);
        assert!(
            !codes(&findings).contains(&"unreachable_node"),
            "boundary event must not be flagged unreachable: {:?}",
            findings
        );
    }

    #[test]
    fn missing_default_branch_is_critical() {
        let conditions = |a: &str| Some(EdgeCondition::Expression(format!("x == \"{}\"", a)));
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![("a", conditions("a")), ("b", conditions("b"))],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        let f = finding(&findings, "missing_default_branch");
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.node_id.as_deref(), Some("gw"));
    }

    #[test]
    fn complement_pair_xor_is_not_missing_default_branch() {
        // `x == "a"` / `x != "a"` on the same variable: mutually exclusive
        // AND exhaustive — exactly one is true for every value, so the
        // gateway can never get stuck for lack of a default branch.
        let both_expressions = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![
                    (
                        "a",
                        Some(EdgeCondition::Expression("x == \"a\"".to_string())),
                    ),
                    (
                        "b",
                        Some(EdgeCondition::Expression("x != \"a\"".to_string())),
                    ),
                ],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
        ]);
        assert!(!codes(&validate_definition(&both_expressions)).contains(&"missing_default_branch"));

        // Mixed forms count too: VariableEq IS `x == "a"` (the DSL lowers
        // variableEq to that canonical expression).
        let mixed = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![
                    (
                        "a",
                        Some(EdgeCondition::VariableEq {
                            key: "x".to_string(),
                            value: "a".to_string(),
                        }),
                    ),
                    (
                        "b",
                        Some(EdgeCondition::Expression("x != \"a\"".to_string())),
                    ),
                ],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
        ]);
        assert!(!codes(&validate_definition(&mixed)).contains(&"missing_default_branch"));
    }

    #[test]
    fn non_complement_pair_xor_is_still_missing_default_branch() {
        // Different variables: `x == "a"` / `y != "a"` can both be false
        // (x = "b", y = "a"), so the Critical finding stays.
        let different_lhs = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![
                    (
                        "a",
                        Some(EdgeCondition::Expression("x == \"a\"".to_string())),
                    ),
                    (
                        "b",
                        Some(EdgeCondition::Expression("y != \"a\"".to_string())),
                    ),
                ],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
        ]);
        assert_eq!(
            finding(
                &validate_definition(&different_lhs),
                "missing_default_branch"
            )
            .severity,
            Severity::Critical
        );
        // Different literals: `x == "a"` / `x != "b"` can both be false
        // (x = "c") — not a complement.
        let different_literal = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![
                    (
                        "a",
                        Some(EdgeCondition::Expression("x == \"a\"".to_string())),
                    ),
                    (
                        "b",
                        Some(EdgeCondition::Expression("x != \"b\"".to_string())),
                    ),
                ],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
        ]);
        assert_eq!(
            finding(
                &validate_definition(&different_literal),
                "missing_default_branch"
            )
            .severity,
            Severity::Critical
        );
    }

    #[test]
    fn dead_branch_after_unconditional_is_warning() {
        // The runtime evaluator takes the FIRST unconditional edge and
        // returns immediately, so a conditional edge listed after it can
        // never be taken.
        let expr = || Some(EdgeCondition::Expression("x == \"a\"".to_string()));
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![("always", None), ("never", expr())],
            ),
            ("always", NodeType::End, vec![]),
            ("never", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        let dead = finding(&findings, "dead_branch_after_unconditional");
        assert_eq!(dead.severity, Severity::Warning);
        assert_eq!(dead.node_id.as_deref(), Some("never"));
        assert!(dead.message.contains("'gw'"));
        assert!(dead.message.contains("'always'"));

        // A default edge listed after an unconditional edge is equally
        // dead (the default is only consulted when the loop finishes).
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![("always", None), ("fallback", Some(EdgeCondition::Default))],
            ),
            ("always", NodeType::End, vec![]),
            ("fallback", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        let dead = finding(&findings, "dead_branch_after_unconditional");
        assert_eq!(dead.node_id.as_deref(), Some("fallback"));
        assert!(dead.message.contains("default"));

        // Order matters: the conditional edge FIRST is fine — the
        // unconditional edge is only consulted after no condition matched.
        let ordered = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![("never", expr()), ("always", None)],
            ),
            ("always", NodeType::End, vec![]),
            ("never", NodeType::End, vec![]),
        ]);
        assert!(!codes(&validate_definition(&ordered)).contains(&"dead_branch_after_unconditional"));
    }

    #[test]
    fn gateway_with_default_or_unconditional_edge_is_not_flagged() {
        let cond = || Some(EdgeCondition::Expression("x == \"a\"".to_string()));
        // With a default edge.
        let with_default = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![("a", cond()), ("b", Some(EdgeCondition::Default))],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
        ]);
        assert!(!codes(&validate_definition(&with_default)).contains(&"missing_default_branch"));
        // With an unconditional edge.
        let with_unconditional = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![("a", cond()), ("b", None)],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
        ]);
        assert!(
            !codes(&validate_definition(&with_unconditional)).contains(&"missing_default_branch")
        );
    }

    #[test]
    fn join_in_cycle_is_warning_and_cycle_is_warning() {
        // fork -> a -> join; join -> a (cycle a <-> join); fork -> b -> join.
        // The validator cannot distinguish a genuinely hazardous re-entry
        // shape from a clean fork -> join -> loop-back cycle, so the honest
        // outcome is an advisory Warning (the runtime re-arms joins).
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("fork", None)]),
            (
                "fork",
                NodeType::ParallelFork,
                vec![("a", None), ("b", None)],
            ),
            ("a", external_task("x", 3, 60), vec![("join", None)]),
            ("b", external_task("x", 3, 60), vec![("join", None)]),
            (
                "join",
                NodeType::ParallelJoin { expected: 2 },
                vec![("a", None)],
            ),
        ]);
        let findings = validate_definition(&def);
        let join = finding(&findings, "join_in_cycle");
        assert_eq!(join.severity, Severity::Warning);
        assert_eq!(join.node_id.as_deref(), Some("join"));
        // The message states the accurate hazard (re-arm per iteration,
        // uneven arrivals), not the disproven deadlock claim.
        assert!(join.message.contains("re-arm"));
        assert!(!join.message.contains("deadlock"));
        let cycle = finding(&findings, "cycle_detected");
        assert_eq!(cycle.severity, Severity::Warning);
        // The cycle members are listed in the message.
        assert!(cycle.message.contains("a -> join") || cycle.message.contains("join -> a"));
    }

    #[test]
    fn clean_cyclic_parallel_flow_has_no_critical_findings() {
        // fork -> branches -> join -> loop back to the fork's predecessor:
        // the canonical VALID cyclic parallel flow. The runtime arms a
        // fresh join group for every fork execution, so only advisory
        // warnings are reported.
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("pre", None)]),
            ("pre", external_task("x", 3, 60), vec![("fork", None)]),
            (
                "fork",
                NodeType::ParallelFork,
                vec![("a", None), ("b", None)],
            ),
            ("a", external_task("x", 3, 60), vec![("join", None)]),
            ("b", external_task("x", 3, 60), vec![("join", None)]),
            (
                "join",
                NodeType::ParallelJoin { expected: 2 },
                vec![("pre", None)],
            ),
        ]);
        let findings = validate_definition(&def);
        assert!(
            !findings.iter().any(|f| f.severity == Severity::Critical),
            "clean cyclic parallel flow must not be invalid: {:?}",
            findings
        );
        assert_eq!(
            finding(&findings, "join_in_cycle").severity,
            Severity::Warning
        );
        assert_eq!(
            finding(&findings, "cycle_detected").severity,
            Severity::Warning
        );
    }

    #[test]
    fn cycle_without_join_is_only_a_warning() {
        // start -> loop1 -> loop2 -> loop1, plus an exit from loop2.
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("loop1", None)]),
            ("loop1", external_task("x", 3, 60), vec![("loop2", None)]),
            (
                "loop2",
                external_task("x", 3, 60),
                vec![("loop1", None), ("end", None)],
            ),
            ("end", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        assert!(codes(&findings).contains(&"cycle_detected"));
        assert!(!codes(&findings).contains(&"join_in_cycle"));
        assert_eq!(
            finding(&findings, "cycle_detected").severity,
            Severity::Warning
        );
    }

    #[test]
    fn invalid_task_config_flags_empty_task_type_and_negative_retries() {
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("t1", None)]),
            ("t1", external_task("   ", 3, 60), vec![("t2", None)]),
            ("t2", external_task("ok", -1, 0), vec![("end", None)]),
            ("end", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        let invalid: Vec<_> = findings
            .iter()
            .filter(|f| f.code == "invalid_task_config")
            .collect();
        assert_eq!(invalid.len(), 2);
        assert!(invalid.iter().any(|f| f.node_id.as_deref() == Some("t1")));
        assert!(invalid.iter().any(|f| f.node_id.as_deref() == Some("t2")));
        assert!(invalid.iter().all(|f| f.severity == Severity::Critical));
        // t2 also has timeout_secs == 0 -> separate warning code.
        let timeout = finding(&findings, "task_timeout_zero");
        assert_eq!(timeout.severity, Severity::Warning);
        assert_eq!(timeout.node_id.as_deref(), Some("t2"));
    }

    #[test]
    fn duplicate_form_field_is_warning() {
        let fields = || {
            Some(vec![
                form_field("amount"),
                form_field("currency"),
                form_field("amount"),
            ])
        };
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("review", None)]),
            (
                "review",
                NodeType::UserTask {
                    form_key: None,
                    form_fields: fields(),
                },
                vec![("end", None)],
            ),
            ("end", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        let duplicates: Vec<_> = findings
            .iter()
            .filter(|f| f.code == "duplicate_form_field")
            .collect();
        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].severity, Severity::Warning);
        assert_eq!(duplicates[0].node_id.as_deref(), Some("review"));
        assert!(duplicates[0].message.contains("amount"));
    }

    #[test]
    fn condition_variables_are_reported_as_info_contract() {
        let eq = |expr: &str| Some(EdgeCondition::Expression(expr.to_string()));
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![
                    (
                        "a",
                        Some(EdgeCondition::VariableEq {
                            key: "amount".to_string(),
                            value: "100".to_string(),
                        }),
                    ),
                    ("b", eq("amount > 100")),
                    ("c", eq("x == \"lit\"")),
                ],
            ),
            ("a", NodeType::End, vec![]),
            ("b", NodeType::End, vec![]),
            ("c", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        let variables: Vec<_> = findings
            .iter()
            .filter(|f| f.code == "condition_variable_scope")
            .collect();
        assert_eq!(variables.len(), 2);
        assert!(variables
            .iter()
            .all(|f| f.severity == Severity::Info && f.node_id.is_none()));
        let amount = variables
            .iter()
            .find(|f| f.message.contains("'amount'"))
            .unwrap();
        // VariableEq key + expression operand: two references.
        assert!(amount.message.contains("referenced by 2"));
        let x = variables
            .iter()
            .find(|f| f.message.contains("'x'"))
            .unwrap();
        assert!(x.message.contains("referenced by 1"));
    }

    #[test]
    fn findings_are_sorted_by_severity_then_code() {
        let def = make_def(vec![
            ("start", NodeType::Start, vec![("gw", None)]),
            (
                "gw",
                NodeType::ExclusiveGateway,
                vec![("a", Some(EdgeCondition::Expression("x == \"a\"".into())))],
            ),
            ("a", NodeType::End, vec![]),
        ]);
        let findings = validate_definition(&def);
        // missing_default_branch (Critical) before condition_variable_scope (Info).
        assert_eq!(
            codes(&findings),
            vec!["missing_default_branch", "condition_variable_scope"]
        );
    }

    #[test]
    fn compiled_bpmn_definitions_validate_clean() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="ok" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="work">
      <extensionElements>
        <taskDefinition type="orders"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="work"/>
    <sequenceFlow id="f2" sourceRef="work" targetRef="end"/>
  </process>
</definitions>"#;
        let def = crate::parse_and_compile(xml).expect("compiles");
        let findings = validate_definition(&def);
        assert!(
            findings.is_empty(),
            "expected clean validation, got {:?}",
            findings
        );
    }
}
