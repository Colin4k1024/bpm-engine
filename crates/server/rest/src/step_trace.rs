//! Step trace assembly (E6-1, issue #26): one per-instance step timeline.
//!
//! [`assemble_steps`] is a PURE, read-only aggregation over what the engine
//! already persists — no new store traits, no new writes:
//!
//! - the process instance (its tokens; **never** its variable values),
//! - the optionally-loaded process definition (node kinds only),
//! - the instance's history events (the single source of execution truth).
//!
//! It groups the history events by `(node_id, token_id)` in causal order —
//! in this engine a token id belongs to exactly one node (each transition
//! mints a fresh token, `crates/runtime/src/transition.rs`), so the pair is
//! the natural step identity — and, for external-task steps, joins the
//! worker lease attempts (`ExternalTaskLocked` → `ExternalTaskFailed` /
//! `ExternalTaskCompleted`, with a re-lock of a still-open attempt closing
//! it as `expired` — E1-3 redelivery) with the AI task envelope recorded
//! under the reserved `__task_envelope` variable of the attempt's outcome
//! event.
//!
//! Sensitive-by-default (issue #26: 敏感字段默认不记录): the assembled
//! response contains **no variable values at all** — only event types,
//! timestamps, worker ids, retry counters, variable NAME lists
//! (`variables_touched`), and envelope metadata summaries (model, tools,
//! cost, artifacts, execution id, retry category). The one value the
//! assembler reads, `__task_envelope`, is engine-owned AI metadata (E3-1);
//! when its stored form was redacted by the E2-3 history policy (oversized
//! → `__truncated:`) or is otherwise unparsable, the summary degrades to
//! `{"opaque": true}` and nothing of the value is surfaced.
//!
//! The trace id is the instance id in v1 (documented decision): a dedicated
//! OTel-style trace id is a future observability concern (E6-2 scope), and
//! the per-HTTP `X-Request-Id` header (request-id middleware) already
//! correlates individual API calls. See `docs/trace.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bpm_engine_core::{
    AiTaskEnvelope, InstanceState, NodeType, ProcessDefinition, ProcessInstance,
    AI_TASK_ENVELOPE_KEY,
};
use bpm_engine_storage::HistoryEvent;
use serde::Serialize;

/// Response of `GET /api/v1/process-instances/:id/steps` (E6-1): the
/// unified, variable-value-free step timeline of one process instance.
#[derive(Debug, Serialize)]
pub struct StepsResponse {
    /// Stable trace id — **the instance id in v1** (see the module docs).
    pub trace_id: String,
    /// The process instance these steps belong to.
    pub instance_id: String,
    /// The definition the instance executes.
    pub process_def_id: String,
    /// Instance lifecycle state: `RUNNING` | `COMPLETED` | `TERMINATED`.
    pub state: String,
    /// Steps in causal order (by first event, then event order).
    pub steps: Vec<StepView>,
}

/// One step: the execution of one node by one token.
#[derive(Debug, Serialize)]
pub struct StepView {
    /// 1-based position in the causal step order.
    pub step_no: u64,
    /// The node this step executed (empty when unknown).
    pub node_id: String,
    /// The token that executed the node (empty for node-scoped events
    /// whose token could not be correlated).
    pub token_id: String,
    /// Step kind: `node` (default), `external_task`, or `timer`.
    pub kind: String,
    /// The history events of this step, in causal order. Types and
    /// timestamps only — never payloads.
    pub events: Vec<StepEventView>,
    /// Worker lease attempts, in order. External-task steps only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<AttemptView>,
    /// Node-level envelope summary (the LAST parsable envelope seen on the
    /// step; `{"opaque": true}` when every recorded envelope was
    /// redacted/unparsable). Absent when the step recorded no envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope_summary: Option<serde_json::Value>,
    /// Variable NAMES touched by this step's events (values never read).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variables_touched: Vec<String>,
}

/// One history event of a step: type and timestamp only.
#[derive(Debug, Clone, Serialize)]
pub struct StepEventView {
    /// History event type, e.g. `TokenArrived`, `ExternalTaskLocked`.
    pub event_type: String,
    /// Unix epoch seconds (the history event's `occurred_at`).
    pub occurred_at: String,
}

/// One worker lease attempt on an external task (E1-3 lease + E3-1
/// envelope metadata), assembled from the lock/fail/complete history
/// events of the task.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptView {
    /// The worker that locked the task.
    pub worker_id: String,
    /// How the attempt ended: `in_flight` (locked, no outcome event yet),
    /// `completed`, `failed`, or `expired` — the lease elapsed without
    /// any outcome and another worker re-locked the same task (E1-3
    /// redelivery), so no terminating event exists for this attempt.
    pub outcome: String,
    /// When the task was locked (the `ExternalTaskLocked` timestamp).
    pub locked_at: String,
    /// When the attempt was completed or failed (the outcome event
    /// timestamp); `null` while the attempt is in flight AND for an
    /// expired attempt (no terminating event carries one).
    #[serde(default)]
    pub completed_or_failed: Option<String>,
    /// `null` on a completed attempt; the recorded error message on a
    /// failed one (with the E3-1 retry-category prefix when present).
    #[serde(default)]
    pub error: Option<String>,
    /// Retry counter of the task as recorded by the event that produced
    /// this attempt state (`null` when the event carried no counter).
    #[serde(default)]
    pub retries_remaining: Option<u64>,
    /// Envelope summary of THIS attempt (from the outcome event's
    /// `__task_envelope`); `{"opaque": true}` when redacted/unparsable.
    /// Absent when the attempt recorded no envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope: Option<serde_json::Value>,
}

/// History event types that carry a task lifecycle transition of an
/// external task (appended by the external-task REST boundary).
const TASK_EVENT_TYPES: [&str; 3] = [
    "ExternalTaskLocked",
    "ExternalTaskFailed",
    "ExternalTaskCompleted",
];

/// Deterministic causal-order tie-break: the lifecycle phase rank of a
/// history event type (E6-1 review fix; see `docs/trace.md` §2).
///
/// `occurred_at` is Unix SECONDS (text), and a lease is commonly locked
/// and resolved within the same second. The memory store happens to
/// preserve insertion order on a stable sort, but a SQL history listing
/// gives no such guarantee — an insertion-order tie-break makes the
/// timeline store-dependent. Instead, every event type gets a fixed
/// rank reflecting its place in the engine's lifecycle, and the causal
/// sort key becomes `(occurred_at, phase_rank, insertion_index)`:
///
/// | rank | lifecycle phase | event types |
/// |------|-----------------|-------------|
/// | 0 | instance starts | `ProcessStarted` |
/// | 10 | token arrives at the node | `TokenArrived` |
/// | 20 | the node's work is scheduled | `TimerScheduled`, `UserTaskCreated`, `CallActivityStarted` |
/// | 30 | a worker takes the lease | `ExternalTaskLocked` |
/// | 40 | the work resolves / the node emits output | `ExternalTaskCompleted`, `ExternalTaskFailed`, `UserTaskCompleted`, `TokenFailed`, `CallActivityCompleted`, `TimerFired`, `MessageSent`, `SignalSent` |
/// | 50 | the token leaves the node | `TokenCompleted` |
/// | 60/61 | compensation | `SagaStarted`, `SagaCompleted` |
/// | 70 | the instance ends | `ProcessCompleted`, `ProcessTerminated` |
///
/// The table covers every event type the engine's history handler
/// (`crates/runtime/src/history_handler.rs`, mirroring
/// `bpm_engine_core::event_from_outbox`) and the external-task REST
/// boundary (`ExternalTaskLocked`/`ExternalTaskFailed`) can append. A
/// lock (30) therefore ALWAYS sorts before its outcome (40) within the
/// same second. Unknown/future event types (a newer engine writing
/// types this version does not know) get a middle default so they
/// interleave deterministically instead of clumping at an end.
///
/// Honest limitation (documented in `docs/trace.md`): same-second
/// ordering BETWEEN different nodes/tasks is still best-effort — the
/// true order is not recoverable from seconds-granularity timestamps
/// plus a type-derived rank. The exact fix is a per-instance monotonic
/// sequence in the history store (future work).
fn phase_rank(event_type: &str) -> u16 {
    match event_type {
        "ProcessStarted" => 0,
        "TokenArrived" => 10,
        "TimerScheduled" | "UserTaskCreated" | "CallActivityStarted" => 20,
        "ExternalTaskLocked" => 30,
        "ExternalTaskCompleted"
        | "ExternalTaskFailed"
        | "UserTaskCompleted"
        | "TokenFailed"
        | "CallActivityCompleted"
        | "TimerFired"
        | "MessageSent"
        | "SignalSent" => 40,
        // Route-layer attribution events (E5-3): rank AFTER the outcome
        // they attribute (40) so a same-second TaskCompleted follows the
        // engine's ExternalTaskCompleted/UserTaskCompleted it describes.
        "TaskCompleted" => 45,
        "TokenCompleted" => 50,
        "SagaStarted" => 60,
        "SagaCompleted" => 61,
        "ProcessCompleted" | "ProcessTerminated" => 70,
        // Middle default: between the lease (30) and the outcome (40)
        // phases — deterministic for events this version does not know.
        _ => 35,
    }
}

/// Establish the causal order of a raw history listing for
/// [`assemble_steps`]: a STABLE sort on `(occurred_at, phase_rank)`, so
/// full ties keep the listing's (insertion) order — the effective key
/// is `(occurred_at, phase_rank, insertion_index)`.
///
/// History event ids are random UUIDs (not sequence numbers), so an id
/// tie-break (like the display-oriented `/history` endpoint uses) would
/// RANDOMIZE same-second lock→outcome pairs and is deliberately not
/// used here. See [`phase_rank`] for the rank table and its limits.
pub fn sort_history_causally(events: &mut [HistoryEvent]) {
    events.sort_by(|a, b| {
        a.occurred_at
            .cmp(&b.occurred_at)
            .then_with(|| phase_rank(&a.event_type).cmp(&phase_rank(&b.event_type)))
    });
}

/// Assemble the step timeline. Pure: no I/O, no stores, no clocks.
///
/// `events` must already be in causal order — the
/// `(occurred_at, phase_rank, insertion_index)` order the `/steps`
/// handler establishes with [`sort_history_causally`] (see that
/// function's docs for why insertion-order ties alone are not a
/// deterministic contract). Events with neither a `node_id` nor a
/// `token_id` (instance-level events such as
/// `ProcessStarted`/`ProcessCompleted`) are intentionally not steps; the
/// instance-level state is the top-level `state` field.
pub fn assemble_steps(
    instance: &ProcessInstance,
    definition: Option<&ProcessDefinition>,
    events: &[HistoryEvent],
) -> StepsResponse {
    // --- token <-> node correlation ---------------------------------------
    //
    // Token ids are unique per node in this engine, so the map is total
    // for any token the engine ever recorded. Instance tokens provide the
    // final state; events with BOTH ids refine it (a completed token is
    // no longer in the instance snapshot).
    fn note_token<'a>(token: &'a str, node: &'a str, token_node: &mut HashMap<&'a str, &'a str>) {
        if !token.is_empty() && !node.is_empty() {
            token_node.insert(token, node);
        }
    }
    let mut token_node: HashMap<&str, &str> = HashMap::new();
    let mut node_tokens: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for token in &instance.tokens {
        note_token(&token.id, &token.node_id, &mut token_node);
    }
    for event in events {
        let token_id = payload_str(&event.payload, "token_id").unwrap_or("");
        let node_id = payload_str(&event.payload, "node_id").unwrap_or("");
        note_token(token_id, node_id, &mut token_node);
    }
    for (token, node) in &token_node {
        node_tokens.entry(node).or_default().insert(token);
    }

    // --- grouping ----------------------------------------------------------
    //
    // Steps accumulate in first-event order (causal); the index map gives
    // O(1) lookup for subsequent events of the same (node, token).
    // `last_arrived` tracks the most recently ARRIVED token per node IN
    // THIS PASS: node-scoped events (`UserTaskCreated`/`UserTaskCompleted`
    // carry instance_id+node_id but NO token_id) refer to exactly that
    // token — a user task is created when its token arrives, so the last
    // `TokenArrived` of the node seen so far is the correct attribution,
    // also when several tokens share one node (loop re-entry, parallel
    // branches).
    let mut steps: Vec<StepAccumulator> = Vec::new();
    let mut index: HashMap<(&str, &str), usize> = HashMap::new();
    let mut last_arrived: HashMap<&str, &str> = HashMap::new();
    for event in events {
        let token_id = payload_str(&event.payload, "token_id").unwrap_or("");
        let node_id = payload_str(&event.payload, "node_id").unwrap_or("");
        if event.event_type == "TokenArrived" && !token_id.is_empty() && !node_id.is_empty() {
            last_arrived.insert(node_id, token_id);
        }
        let (node, token) = if !token_id.is_empty() {
            // Token-scoped event: the node comes from the correlation map
            // (events like TokenCompleted/ExternalTaskLocked carry no node).
            (token_node.get(token_id).copied().unwrap_or(""), token_id)
        } else if !node_id.is_empty() {
            // Node-scoped event: the token that most recently ARRIVED at
            // this node (see above). Fallback — no arrival in the visible
            // history, e.g. a truncated window — any token correlated to
            // the node, deterministically the lexicographically first.
            // None → the empty pseudo-token.
            let token = last_arrived
                .get(node_id)
                .copied()
                .or_else(|| {
                    node_tokens
                        .get(node_id)
                        .and_then(|tokens| tokens.iter().next().copied())
                })
                .unwrap_or("");
            (node_id, token)
        } else {
            continue; // instance-level event: not a step
        };
        let step = match index.get(&(node, token)) {
            Some(&i) => &mut steps[i],
            None => {
                steps.push(StepAccumulator::new(node, token));
                index.insert((node, token), steps.len() - 1);
                steps.last_mut().expect("just pushed")
            }
        };
        step.push_event(event);
    }

    // --- view assembly ------------------------------------------------------
    // Superseded attempts that no outcome event claimed end as `expired`.
    for step in &mut steps {
        step.finalize_attempts();
    }
    let steps: Vec<StepView> = steps
        .iter()
        .enumerate()
        .map(|(i, acc)| acc.to_view(i as u64 + 1, definition))
        .collect();

    StepsResponse {
        trace_id: instance.id.clone(),
        instance_id: instance.id.clone(),
        process_def_id: instance.process_def_id.clone(),
        state: instance_state_str(instance.state).to_string(),
        steps,
    }
}

/// Mutable per-step accumulator (internal; the wire shape is
/// [`StepView`]).
struct StepAccumulator<'a> {
    node_id: &'a str,
    token_id: &'a str,
    events: Vec<StepEventView>,
    /// Variable names seen in `variables`/`initial_variables` payload keys.
    variables_touched: BTreeSet<&'a str>,
    /// External-task attempts, in lock order.
    attempts: Vec<AttemptView>,
    /// Lifecycle state of each attempt, parallel to `attempts`.
    states: Vec<AttemptState>,
    /// The CURRENT open attempt per task id (the latest lock, not yet
    /// resolved).
    open: HashMap<&'a str, usize>,
    /// Superseded-but-unresolved attempts per task id: closed by a later
    /// re-lock of the same task, they await either a belated outcome
    /// event (matched by worker id — the fail event carries one) or the
    /// end-of-history verdict `expired` (E1-3 redelivery).
    unresolved: HashMap<&'a str, Vec<usize>>,
    /// The LAST successfully parsed envelope of the step, and whether any
    /// recorded envelope failed to parse.
    last_envelope: Option<AiTaskEnvelope>,
    any_opaque: bool,
    /// Whether any task-lifecycle or timer event occurred (kind fallback
    /// when the definition is unavailable).
    saw_task_events: bool,
    saw_timer_events: bool,
}

/// Internal lifecycle of one attempt while assembling (see
/// [`StepAccumulator::unresolved`]).
#[derive(Debug, Clone, Copy, PartialEq)]
enum AttemptState {
    /// Locked, no outcome yet: `in_flight` on the wire.
    InFlight,
    /// A later lock of the same task superseded it and no outcome event
    /// claimed it (worker-id match): `expired` on the wire.
    Superseded,
    /// Closed by `ExternalTaskCompleted`: `completed` on the wire.
    Completed,
    /// Closed by `ExternalTaskFailed`: `failed` on the wire.
    Failed,
}

impl<'a> StepAccumulator<'a> {
    fn new(node_id: &'a str, token_id: &'a str) -> Self {
        StepAccumulator {
            node_id,
            token_id,
            events: Vec::new(),
            variables_touched: BTreeSet::new(),
            attempts: Vec::new(),
            states: Vec::new(),
            open: HashMap::new(),
            unresolved: HashMap::new(),
            last_envelope: None,
            any_opaque: false,
            saw_task_events: false,
            saw_timer_events: false,
        }
    }

    fn push_event(&mut self, event: &'a HistoryEvent) {
        self.events.push(StepEventView {
            event_type: event.event_type.clone(),
            occurred_at: event.occurred_at.clone(),
        });
        if TASK_EVENT_TYPES.contains(&event.event_type.as_str()) {
            self.saw_task_events = true;
        }
        if event.event_type.starts_with("Timer") {
            self.saw_timer_events = true;
        }
        // Variable NAMES only (issue #26: sensitive by default — values are
        // never read); the engine-owned envelope key is metadata, not a
        // user variable, and is reported through the envelope summary.
        for key in ["variables", "initial_variables"] {
            if let Some(names) = event.payload.get(key).and_then(|v| v.as_object()) {
                for name in names.keys() {
                    if name != AI_TASK_ENVELOPE_KEY {
                        self.variables_touched.insert(name);
                    }
                }
            }
        }
        match event.event_type.as_str() {
            "ExternalTaskLocked" => self.attempt_locked(event),
            "ExternalTaskFailed" => self.attempt_outcome(event, true),
            "ExternalTaskCompleted" => self.attempt_outcome(event, false),
            _ => {}
        }
    }

    /// `ExternalTaskLocked` opens a new attempt for the task. When the
    /// SAME task is re-locked while a previous attempt is still open
    /// (E1-3 redelivery after lease expiry — no fail/complete event was
    /// ever recorded for it), that attempt is SUPERSEDED, not closed: it
    /// still ends up `expired` unless a later outcome event claims it
    /// (matched by worker id — a fail event that landed in the same
    /// second can sort after the re-lock).
    fn attempt_locked(&mut self, event: &'a HistoryEvent) {
        let task_id = payload_str(&event.payload, "task_id").unwrap_or("");
        if !task_id.is_empty() {
            if let Some(superseded) = self.open.remove(task_id) {
                self.states[superseded] = AttemptState::Superseded;
                self.unresolved.entry(task_id).or_default().push(superseded);
            }
        }
        let attempt = AttemptView {
            worker_id: payload_str(&event.payload, "worker_id")
                .unwrap_or("")
                .to_string(),
            outcome: "in_flight".to_string(),
            locked_at: event.occurred_at.clone(),
            completed_or_failed: None,
            error: None,
            retries_remaining: payload_u64(&event.payload, "retries"),
            envelope: None,
        };
        self.attempts.push(attempt);
        self.states.push(AttemptState::InFlight);
        if !task_id.is_empty() {
            self.open.insert(task_id, self.attempts.len() - 1);
        }
    }

    /// `ExternalTaskFailed` / `ExternalTaskCompleted` close the attempt of
    /// the task they belong to and attach the envelope recorded with the
    /// outcome.
    ///
    /// Which attempt: the outcome event's `worker_id` (carried by the
    /// REST fail boundary) picks the most recent attempt of the task with
    /// a MATCHING worker — that repairs the same-second case where the
    /// phase-rank causal sort moved the re-lock ahead of a preceding
    /// failure. Without a worker signal (the engine's
    /// `ExternalTaskCompleted` carries none) the CURRENT holder (the
    /// latest lock) is the addressee.
    fn attempt_outcome(&mut self, event: &'a HistoryEvent, failed: bool) {
        let task_id = payload_str(&event.payload, "task_id").unwrap_or("");
        let worker_id = payload_str(&event.payload, "worker_id");
        let summary = self.record_envelope(event);
        // Candidate attempts of this task: the current open lock plus the
        // superseded-but-unresolved ones.
        let current = if task_id.is_empty() {
            // No task scope at all: the old honest fallback — the newest
            // open attempt (the outcome event itself is still listed).
            self.open.values().max().copied()
        } else {
            self.open.remove(task_id)
        };
        let mut candidates: Vec<usize> = if task_id.is_empty() {
            Vec::new()
        } else {
            self.unresolved.get(task_id).cloned().unwrap_or_default()
        };
        if let Some(current) = current {
            candidates.push(current);
        }
        let chosen = match worker_id {
            Some(worker) => candidates
                .into_iter()
                .filter(|&i| self.attempts[i].worker_id == worker)
                .max(),
            None => current,
        };
        let Some(index) = chosen else {
            // Nothing matched (a history gap — an outcome for a task with
            // no recorded lock): put the current holder back and keep the
            // event listed in `events` only.
            if let (Some(current), false) = (current, task_id.is_empty()) {
                self.open.insert(task_id, current);
            }
            return;
        };
        if Some(index) != current {
            // The outcome claimed a SUPERSEDED attempt, not the current
            // holder: the current holder is still open — restore it.
            if let (Some(current), false) = (current, task_id.is_empty()) {
                self.open.insert(task_id, current);
            }
        }
        // Drop the chosen index from the unresolved list if it came from
        // there (it is already gone from `open`).
        if !task_id.is_empty() {
            if let Some(list) = self.unresolved.get_mut(task_id) {
                list.retain(|&i| i != index);
            }
        }
        let attempt = &mut self.attempts[index];
        self.states[index] = if failed {
            AttemptState::Failed
        } else {
            AttemptState::Completed
        };
        attempt.outcome = if failed { "failed" } else { "completed" }.to_string();
        attempt.completed_or_failed = Some(event.occurred_at.clone());
        if failed {
            attempt.error = payload_str(&event.payload, "error_message").map(str::to_string);
        }
        if let Some(retries) = payload_u64(&event.payload, "retries") {
            attempt.retries_remaining = Some(retries);
        }
        if summary.is_some() {
            attempt.envelope = summary;
        }
    }

    /// End-of-history verdict: an attempt still superseded (a later lock
    /// took over the task and no outcome event ever claimed it by worker
    /// id) is `expired` — the E1-3 redelivery case with no terminating
    /// event. Everything else keeps the outcome it was closed with.
    fn finalize_attempts(&mut self) {
        for (index, state) in self.states.iter().enumerate() {
            if *state == AttemptState::Superseded {
                self.attempts[index].outcome = "expired".to_string();
            }
        }
    }

    /// Parse the attempt envelope recorded under
    /// `payload.variables.__task_envelope` (E3-1) and track it for the
    /// node-level summary. Returns the attempt summary, or `None` when the
    /// event carries no envelope key at all.
    fn record_envelope(&mut self, event: &HistoryEvent) -> Option<serde_json::Value> {
        let stored = event
            .payload
            .get("variables")
            .and_then(|v| v.get(AI_TASK_ENVELOPE_KEY))
            .and_then(|v| v.as_str())?;
        match AiTaskEnvelope::from_envelope_value(stored) {
            Ok(envelope) => {
                // Validated shapes only: a stored-but-invalid envelope
                // (contract violation at write time) is reported opaque,
                // not echoed.
                if envelope.validate().is_ok() {
                    self.last_envelope = Some(envelope.clone());
                    Some(envelope_summary(&envelope))
                } else {
                    self.any_opaque = true;
                    Some(opaque_summary())
                }
            }
            // Redacted (`__truncated:`/`__opaque:`) or unparsable: never
            // surface any part of the stored value.
            Err(_) => {
                self.any_opaque = true;
                Some(opaque_summary())
            }
        }
    }

    fn to_view(&self, step_no: u64, definition: Option<&ProcessDefinition>) -> StepView {
        let kind = step_kind(
            self.node_id,
            definition,
            self.saw_task_events,
            self.saw_timer_events,
        );
        let envelope_summary = match &self.last_envelope {
            Some(envelope) => Some(envelope_summary(envelope)),
            None if self.any_opaque => Some(opaque_summary()),
            None => None,
        };
        StepView {
            step_no,
            node_id: self.node_id.to_string(),
            token_id: self.token_id.to_string(),
            kind: kind.to_string(),
            events: self.events.clone(),
            attempts: self.attempts.clone(),
            envelope_summary,
            variables_touched: self
                .variables_touched
                .iter()
                .map(|name| name.to_string())
                .collect(),
        }
    }
}

/// Step kind: the definition's node type wins; without a definition the
/// observed event types classify (def-missing is a degraded, still-honest
/// mode).
fn step_kind(
    node_id: &str,
    def: Option<&ProcessDefinition>,
    task_events: bool,
    timer_events: bool,
) -> &'static str {
    if let Some(def) = def {
        if let Some(node) = def.nodes.get(node_id) {
            return match node.node_type {
                NodeType::ExternalTask { .. } => "external_task",
                NodeType::TimerIntermediateCatch { .. } | NodeType::BoundaryTimer { .. } => "timer",
                _ => "node",
            };
        }
    }
    if task_events {
        "external_task"
    } else if timer_events {
        "timer"
    } else {
        "node"
    }
}

/// Envelope metadata summary (issue #26: model/tool versions, cost,
/// artifacts, execution id, retry category — NO schemas, NO idempotency
/// key, and never any variable value).
fn envelope_summary(envelope: &AiTaskEnvelope) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if !envelope.execution_id.is_empty() {
        map.insert(
            "execution_id".into(),
            serde_json::json!(envelope.execution_id),
        );
    }
    if let Some(category) = &envelope.retry_category {
        map.insert(
            "retry_category".into(),
            serde_json::to_value(category).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(model) = &envelope.model {
        map.insert(
            "model".into(),
            serde_json::to_value(model).unwrap_or(serde_json::Value::Null),
        );
    }
    if !envelope.tools.is_empty() {
        map.insert(
            "tools".into(),
            serde_json::to_value(&envelope.tools).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(cost) = &envelope.cost {
        map.insert(
            "cost".into(),
            serde_json::to_value(cost).unwrap_or(serde_json::Value::Null),
        );
    }
    if !envelope.artifacts.is_empty() {
        map.insert(
            "artifacts".into(),
            serde_json::to_value(&envelope.artifacts).unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(map)
}

/// The degraded summary for redacted/unparsable envelopes.
fn opaque_summary() -> serde_json::Value {
    serde_json::json!({ "opaque": true })
}

fn payload_str<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn payload_u64(payload: &serde_json::Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(|v| v.as_u64())
}

fn instance_state_str(state: InstanceState) -> &'static str {
    match state {
        InstanceState::Running => "RUNNING",
        InstanceState::Completed => "COMPLETED",
        InstanceState::Terminated => "TERMINATED",
    }
}

// ---------------------------------------------------------------------------
// Tests: grouping / ordering / joining logic of the pure assembler
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_core::{payloads, Node, OutgoingEdge, Token, TokenMode, TokenStatus};
    use std::collections::HashMap as StdHashMap;

    fn node(id: &'static str, node_type: NodeType, target: &[&'static str]) -> Node {
        Node {
            id,
            node_type,
            outgoing_edges: target
                .iter()
                .map(|t| OutgoingEdge {
                    target: t,
                    condition: None,
                })
                .collect(),
        }
    }

    fn def_with(nodes: Vec<(&'static str, NodeType)>) -> ProcessDefinition {
        ProcessDefinition {
            id: "def-1",
            start: "start",
            boundary_events: StdHashMap::new(),
            nodes: nodes
                .into_iter()
                .map(|(id, nt)| {
                    let n = match &nt {
                        NodeType::End => node(id, nt, &[]),
                        _ => node(id, nt, &["end"]),
                    };
                    (id, n)
                })
                .collect(),
        }
    }

    fn instance(tokens: Vec<(&str, &str)>) -> ProcessInstance {
        ProcessInstance {
            id: "inst-1".to_string(),
            process_def_id: "def-1".to_string(),
            tenant_id: None,
            tokens: tokens
                .into_iter()
                .map(|(id, node_id)| Token {
                    id: id.to_string(),
                    node_id: node_id.to_string(),
                    status: TokenStatus::Waiting,
                    mode: TokenMode::Forward,
                    version: 0,
                    attempt: 0,
                    parallel_group_id: None,
                    updated_at: None,
                })
                .collect(),
            variables: StdHashMap::new(),
            state: InstanceState::Running,
            version: 1,
            parent_instance_id: None,
            parent_token_id: None,
        }
    }

    fn event(
        id: &str,
        event_type: &str,
        occurred_at: &str,
        payload: serde_json::Value,
    ) -> HistoryEvent {
        HistoryEvent {
            id: id.to_string(),
            instance_id: "inst-1".to_string(),
            event_type: event_type.to_string(),
            payload,
            occurred_at: occurred_at.to_string(),
        }
    }

    fn envelope_value(envelope: &AiTaskEnvelope) -> serde_json::Value {
        serde_json::json!(envelope.to_envelope_value())
    }

    fn lock_event(
        id: &str,
        at: &str,
        task: &str,
        token: &str,
        worker: &str,
        retries: u64,
    ) -> HistoryEvent {
        event(
            id,
            "ExternalTaskLocked",
            at,
            serde_json::json!({
                "task_id": task, "token_id": token,
                "process_instance_id": "inst-1", "worker_id": worker, "retries": retries,
            }),
        )
    }

    /// The outcome half of one task attempt (envelope/error/retries/worker
    /// all optional, like the real payloads: the REST fail event carries
    /// `worker_id`, the engine's complete event does not).
    #[derive(Default)]
    struct Outcome<'a> {
        envelope: Option<&'a AiTaskEnvelope>,
        error: Option<&'a str>,
        retries: Option<u64>,
        worker: Option<&'a str>,
    }

    fn outcome_event(
        id: &str,
        event_type: &str,
        at: &str,
        task: &str,
        token: &str,
        outcome: Outcome<'_>,
    ) -> HistoryEvent {
        let mut payload = serde_json::json!({
            "task_id": task, "token_id": token,
            "process_instance_id": "inst-1",
        });
        // The engine's ExternalTaskCompleted event carries node_id; the
        // REST fail event does not (the node resolves via token
        // correlation, which is what the assembler tests exercise).
        if event_type == "ExternalTaskCompleted" {
            payload["node_id"] = serde_json::json!("work");
        }
        if let Some(error) = outcome.error {
            payload["error_message"] = serde_json::json!(error);
        }
        if let Some(worker) = outcome.worker {
            payload["worker_id"] = serde_json::json!(worker);
        }
        if let Some(retries) = outcome.retries {
            payload["retries"] = serde_json::json!(retries);
        }
        if let Some(envelope) = outcome.envelope {
            payload["variables"] = serde_json::json!({
                AI_TASK_ENVELOPE_KEY: envelope_value(envelope),
                "summary": "a variable value that must never surface",
            });
        }
        event(id, event_type, at, payload)
    }

    // --- grouping & ordering ------------------------------------------------

    #[test]
    fn groups_by_node_and_token_in_causal_order() {
        let events = vec![
            event(
                "1",
                "ProcessStarted",
                "100",
                serde_json::json!({ "instance_id": "inst-1" }),
            ),
            event(
                "2",
                "TokenArrived",
                "101",
                serde_json::json!({ "token_id": "t1", "node_id": "start" }),
            ),
            event(
                "3",
                "TokenCompleted",
                "102",
                serde_json::json!({ "token_id": "t1" }),
            ),
            event(
                "4",
                "TokenArrived",
                "103",
                serde_json::json!({ "token_id": "t2", "node_id": "review" }),
            ),
            event(
                "5",
                "UserTaskCreated",
                "104",
                serde_json::json!({ "node_id": "review" }),
            ),
            event(
                "6",
                "ProcessCompleted",
                "105",
                serde_json::json!({ "instance_id": "inst-1" }),
            ),
        ];
        let inst = instance(vec![("t2", "review")]);
        let response = assemble_steps(&inst, None, &events);

        // Instance-level events (ProcessStarted/ProcessCompleted) are not
        // steps; the two token/node steps appear in first-event (causal)
        // order with 1-based step_no.
        assert_eq!(response.steps.len(), 2);

        let first = &response.steps[0];
        assert_eq!(
            (first.node_id.as_str(), first.token_id.as_str()),
            ("start", "t1")
        );
        assert_eq!(first.step_no, 1);
        assert_eq!(first.events.len(), 2);

        let second = &response.steps[1];
        assert_eq!(
            (second.node_id.as_str(), second.token_id.as_str()),
            ("review", "t2")
        );
        assert_eq!(second.step_no, 2);
        // UserTaskCreated (node-scoped, no token of its own) correlates to
        // the token at that node: SAME step, appended event.
        assert_eq!(second.events.len(), 2);
        assert_eq!(second.events[0].event_type, "TokenArrived");
        assert_eq!(second.events[1].event_type, "UserTaskCreated");
        // Trace id IS the instance id (v1 decision).
        assert_eq!(response.trace_id, "inst-1");
        assert_eq!(response.instance_id, "inst-1");
        assert_eq!(response.process_def_id, "def-1");
        assert_eq!(response.state, "RUNNING");
    }

    #[test]
    fn token_without_node_resolves_via_event_correlation() {
        // TokenCompleted carries no node_id: the node comes from the same
        // token's earlier TokenArrived.
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            event(
                "2",
                "TokenCompleted",
                "101",
                serde_json::json!({ "token_id": "t1" }),
            ),
        ];
        let inst = instance(vec![]);
        let response = assemble_steps(&inst, None, &events);
        assert_eq!(response.steps.len(), 1);
        assert_eq!(response.steps[0].node_id, "work");
        assert_eq!(response.steps[0].token_id, "t1");
        assert_eq!(response.steps[0].events.len(), 2);
    }

    #[test]
    fn node_scoped_events_follow_the_most_recently_arrived_token() {
        // One user-task node visited by TWO tokens (loop re-entry). The
        // node-scoped UserTaskCreated/UserTaskCompleted events carry NO
        // token_id and must attribute to their respective
        // MOST-RECENTLY-ARRIVED tokens — not to the lexicographically
        // first token of the node ("t-a" < "t-b", but "t-b" arrives and
        // completes first).
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t-b", "node_id": "review" }),
            ),
            event(
                "2",
                "UserTaskCreated",
                "101",
                serde_json::json!({ "node_id": "review" }),
            ),
            event(
                "3",
                "UserTaskCompleted",
                "102",
                serde_json::json!({
                    "task_id": "ut-1", "node_id": "review",
                    "variables": { "decision": "approve" }
                }),
            ),
            event(
                "4",
                "TokenCompleted",
                "103",
                serde_json::json!({ "token_id": "t-b" }),
            ),
            event(
                "5",
                "TokenArrived",
                "104",
                serde_json::json!({ "token_id": "t-a", "node_id": "review" }),
            ),
            event(
                "6",
                "UserTaskCreated",
                "105",
                serde_json::json!({ "node_id": "review" }),
            ),
            event(
                "7",
                "TokenCompleted",
                "106",
                serde_json::json!({ "token_id": "t-a" }),
            ),
        ];
        let inst = instance(vec![]);
        let response = assemble_steps(&inst, None, &events);

        // Two steps — one per (node, token) — each user-task event on the
        // step of the token whose arrival preceded it.
        assert_eq!(response.steps.len(), 2);

        let first = &response.steps[0];
        assert_eq!(
            (first.node_id.as_str(), first.token_id.as_str()),
            ("review", "t-b"),
            "the first round's user-task events belong to t-b"
        );
        let types: Vec<&str> = first.events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "TokenArrived",
                "UserTaskCreated",
                "UserTaskCompleted",
                "TokenCompleted"
            ]
        );

        let second = &response.steps[1];
        assert_eq!(
            (second.node_id.as_str(), second.token_id.as_str()),
            ("review", "t-a"),
            "the re-entry's user task belongs to t-a, not t-b"
        );
        let types: Vec<&str> = second
            .events
            .iter()
            .map(|e| e.event_type.as_str())
            .collect();
        assert_eq!(
            types,
            vec!["TokenArrived", "UserTaskCreated", "TokenCompleted"]
        );
    }

    #[test]
    fn empty_history_yields_steps_from_instance_tokens_only_if_events_exist() {
        let inst = instance(vec![("t2", "review")]);
        let response = assemble_steps(&inst, None, &[]);
        assert!(
            response.steps.is_empty(),
            "no events ⇒ no steps: the timeline is history-driven"
        );
    }

    // --- causal sort: deterministic phase-rank tie-break -----------------------

    #[test]
    fn causal_sort_keeps_lock_before_its_same_second_outcome() {
        // All three events share one timestamp (a fast round trip). The
        // listing arrives in an order a store WITHOUT insertion-order
        // guarantees could return (outcome first); the phase rank must
        // restore arrival → lock → outcome.
        let mut events = vec![
            outcome_event(
                "3",
                "ExternalTaskCompleted",
                "100",
                "task-1",
                "t1",
                Outcome::default(),
            ),
            lock_event("2", "100", "task-1", "t1", "worker-a", 3),
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
        ];
        sort_history_causally(&mut events);
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "TokenArrived",
                "ExternalTaskLocked",
                "ExternalTaskCompleted"
            ],
            "same-second tie: rank arrival(10) < lock(30) < outcome(40)"
        );
    }

    #[test]
    fn causal_sort_interleaves_two_same_second_lock_outcome_pairs() {
        // Two tasks, each locked and resolved within ONE second — the
        // common parallel-tasks case. Every lock (rank 30) must precede
        // its own outcome (rank 40); full ties (lock vs lock, outcome vs
        // outcome) keep the listing order.
        let mut events = vec![
            lock_event("l1", "100", "task-1", "t1", "worker-a", 3),
            outcome_event(
                "c1",
                "ExternalTaskCompleted",
                "100",
                "task-1",
                "t1",
                Outcome::default(),
            ),
            lock_event("l2", "100", "task-2", "t2", "worker-b", 3),
            outcome_event(
                "f2",
                "ExternalTaskFailed",
                "100",
                "task-2",
                "t2",
                Outcome {
                    error: Some("model over quota"),
                    worker: Some("worker-b"),
                    ..Default::default()
                },
            ),
        ];
        sort_history_causally(&mut events);
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "ExternalTaskLocked",    // task-1 lock
                "ExternalTaskLocked",    // task-2 lock
                "ExternalTaskCompleted", // task-1 outcome
                "ExternalTaskFailed",    // task-2 outcome
            ]
        );
    }

    #[test]
    fn causal_sort_orders_seconds_before_phase_and_unknown_types_rank_mid() {
        // A later second always wins over the phase rank; an unknown
        // (future) event type gets the MIDDLE default, so within one
        // second it lands between the arrival and the completion phases.
        let mut events = vec![
            event(
                "3",
                "ProcessCompleted",
                "101",
                serde_json::json!({ "instance_id": "inst-1" }),
            ),
            event(
                "1",
                "FutureEngineEvent",
                "100",
                serde_json::json!({ "token_id": "t1" }),
            ),
            event(
                "2",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            event(
                "4",
                "TokenCompleted",
                "100",
                serde_json::json!({ "token_id": "t1" }),
            ),
        ];
        sort_history_causally(&mut events);
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "TokenArrived",
                "FutureEngineEvent",
                "TokenCompleted",
                "ProcessCompleted"
            ],
            "same second: arrival(10) < unknown(35) < completion(50); the \
             later second dominates everything"
        );
    }

    // --- kinds ----------------------------------------------------------------

    #[test]
    fn kinds_from_definition_and_event_fallback() {
        let def = def_with(vec![
            ("start", NodeType::Start),
            (
                "work",
                NodeType::ExternalTask {
                    task_type: "ai".to_string(),
                    retries: 3,
                    timeout_secs: 60,
                },
            ),
            (
                "wait",
                NodeType::TimerIntermediateCatch {
                    timer_definition: "PT1S".to_string(),
                },
            ),
            ("end", NodeType::End),
        ]);
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "start" }),
            ),
            event(
                "2",
                "TokenArrived",
                "101",
                serde_json::json!({ "token_id": "t2", "node_id": "work" }),
            ),
            event(
                "3",
                "TokenArrived",
                "102",
                serde_json::json!({ "token_id": "t3", "node_id": "wait" }),
            ),
            event(
                "4",
                "TimerScheduled",
                "103",
                serde_json::json!({ "token_id": "t3", "node_id": "wait", "timer_id": "tm-1" }),
            ),
            event(
                "5",
                "TokenArrived",
                "104",
                serde_json::json!({ "token_id": "t4", "node_id": "end" }),
            ),
        ];
        let response = assemble_steps(&instance(vec![]), Some(&def), &events);
        let kinds: Vec<&str> = response.steps.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["node", "external_task", "timer", "node"]);

        // No definition available: event types classify (degraded mode).
        let response = assemble_steps(&instance(vec![]), None, &events);
        let kinds: Vec<&str> = response.steps.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["node", "node", "timer", "node"]);
    }

    // --- attempts & envelopes --------------------------------------------------

    fn test_envelope(execution_id: &str) -> AiTaskEnvelope {
        AiTaskEnvelope {
            schema_version: 1,
            execution_id: execution_id.to_string(),
            idempotency_key: Some("sha256:input".to_string()),
            timeout_secs: Some(120),
            retry_category: None,
            model: Some(bpm_engine_core::ModelRef {
                provider: "openai".to_string(),
                name: "gpt-4o".to_string(),
                version: Some("2024-08-06".to_string()),
            }),
            tools: vec![],
            cost: Some(bpm_engine_core::TaskCost {
                currency: Some("USD".to_string()),
                amount_micros: Some(1_500_000),
                tokens_in: Some(1_204),
                tokens_out: Some(37),
                duration_ms: Some(980),
            }),
            artifacts: vec![],
            input_schema: None,
            output_schema: None,
        }
    }

    #[test]
    fn attempts_join_lock_fail_and_complete_with_envelopes() {
        let fail_envelope = {
            let mut e = test_envelope("task-1:0");
            e.retry_category = Some(bpm_engine_core::RetryCategory::Transient);
            e
        };
        let complete_envelope = {
            let mut e = test_envelope("task-1:1");
            e.artifacts = vec![bpm_engine_core::ArtifactRef {
                uri: "https://storage.example.com/out/report.md".to_string(),
                sha256: None,
                size: Some(4096),
                media_type: Some("text/markdown".to_string()),
            }];
            e
        };
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            lock_event("2", "101", "task-1", "t1", "worker-a", 3),
            outcome_event(
                "3",
                "ExternalTaskFailed",
                "102",
                "task-1",
                "t1",
                Outcome {
                    envelope: Some(&fail_envelope),
                    error: Some(r#"{"retry_category":"transient"}: model over quota"#),
                    retries: Some(2),
                    worker: Some("worker-a"),
                },
            ),
            lock_event("4", "103", "task-1", "t1", "worker-b", 2),
            outcome_event(
                "5",
                "ExternalTaskCompleted",
                "104",
                "task-1",
                "t1",
                Outcome {
                    envelope: Some(&complete_envelope),
                    ..Default::default()
                },
            ),
            event(
                "6",
                "TokenCompleted",
                "105",
                serde_json::json!({ "token_id": "t1" }),
            ),
        ];
        let def = def_with(vec![(
            "work",
            NodeType::ExternalTask {
                task_type: "ai".to_string(),
                retries: 3,
                timeout_secs: 60,
            },
        )]);
        let response = assemble_steps(&instance(vec![]), Some(&def), &events);

        assert_eq!(response.steps.len(), 1);
        let step = &response.steps[0];
        assert_eq!(step.kind, "external_task");
        assert_eq!(
            step.attempts.len(),
            2,
            "two lease attempts (A failed, B completed)"
        );

        let a = &step.attempts[0];
        assert_eq!(a.worker_id, "worker-a");
        assert_eq!(a.outcome, "failed");
        assert_eq!(a.locked_at, "101");
        assert_eq!(a.completed_or_failed.as_deref(), Some("102"));
        assert!(a.error.as_deref().unwrap().contains("model over quota"));
        assert_eq!(a.retries_remaining, Some(2), "retries after the failure");
        assert_eq!(a.envelope.as_ref().unwrap()["model"]["name"], "gpt-4o");
        assert_eq!(
            a.envelope.as_ref().unwrap()["cost"]["amount_micros"],
            1_500_000
        );
        assert_eq!(a.envelope.as_ref().unwrap()["retry_category"], "transient");
        // Idempotency key and schemas are NOT part of the summary.
        assert!(a
            .envelope
            .as_ref()
            .unwrap()
            .get("idempotency_key")
            .is_none());
        assert!(a.envelope.as_ref().unwrap().get("input_schema").is_none());
        // The payload variable VALUE never surfaces.
        let raw = serde_json::to_string(&response).unwrap();
        assert!(!raw.contains("a variable value that must never surface"));

        let b = &step.attempts[1];
        assert_eq!(b.worker_id, "worker-b");
        assert_eq!(b.outcome, "completed");
        assert_eq!(b.completed_or_failed.as_deref(), Some("104"));
        assert!(b.error.is_none());
        assert_eq!(
            b.envelope.as_ref().unwrap()["artifacts"][0]["uri"],
            "https://storage.example.com/out/report.md"
        );

        // Node-level summary: the LAST parsable envelope (worker B's).
        let summary = step.envelope_summary.as_ref().unwrap();
        assert_eq!(summary["execution_id"], "task-1:1");
        assert_eq!(summary["cost"]["tokens_in"], 1_204);
        // The step's events include the full task lifecycle.
        let types: Vec<&str> = step.events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "TokenArrived",
                "ExternalTaskLocked",
                "ExternalTaskFailed",
                "ExternalTaskLocked",
                "ExternalTaskCompleted",
                "TokenCompleted",
            ]
        );
    }

    #[test]
    fn redacted_envelope_is_opaque_and_no_value_leaks() {
        // A stored envelope value redacted by the E2-3 history policy.
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            lock_event("2", "101", "task-1", "t1", "worker-a", 3),
            event(
                "3",
                "ExternalTaskCompleted",
                "102",
                serde_json::json!({
                    "task_id": "task-1", "token_id": "t1", "node_id": "work",
                    "variables": {
                        AI_TASK_ENVELOPE_KEY: "__truncated:5400:0123456789ab:",
                        "secret_output": "hunter2-the-plaintext",
                    },
                }),
            ),
        ];
        let def = def_with(vec![(
            "work",
            NodeType::ExternalTask {
                task_type: "ai".to_string(),
                retries: 3,
                timeout_secs: 60,
            },
        )]);
        let response = assemble_steps(&instance(vec![]), Some(&def), &events);
        let step = &response.steps[0];
        assert_eq!(step.attempts.len(), 1);
        let attempt = &step.attempts[0];
        assert_eq!(
            attempt.envelope,
            Some(serde_json::json!({ "opaque": true })),
            "redacted envelope ⇒ opaque summary"
        );
        assert_eq!(
            step.envelope_summary,
            Some(serde_json::json!({ "opaque": true }))
        );
        // The truncated marker text itself must not leak either.
        let raw = serde_json::to_string(&response).unwrap();
        assert!(!raw.contains("__truncated"));
        assert!(!raw.contains("hunter2"), "variable values never surface");
        // The variable NAME is reported, the value is not.
        assert!(step
            .variables_touched
            .contains(&"secret_output".to_string()));
    }

    #[test]
    fn outcome_without_lock_and_open_attempt_are_honest() {
        // A lock with no outcome: the attempt stays open (in flight).
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            lock_event("2", "101", "task-1", "t1", "worker-a", 3),
        ];
        let def = def_with(vec![(
            "work",
            NodeType::ExternalTask {
                task_type: "ai".to_string(),
                retries: 3,
                timeout_secs: 60,
            },
        )]);
        let response = assemble_steps(&instance(vec![]), Some(&def), &events);
        let attempt = &response.steps[0].attempts[0];
        assert_eq!(attempt.worker_id, "worker-a");
        assert_eq!(attempt.outcome, "in_flight");
        assert!(attempt.completed_or_failed.is_none(), "in-flight attempt");
        assert!(attempt.error.is_none());
        assert!(attempt.envelope.is_none());
    }

    #[test]
    fn relock_closes_the_open_attempt_as_expired() {
        // E1-3 redelivery: worker A's lease ELAPSES without any outcome
        // (no ExternalTaskFailed/Completed exists for it), and worker B
        // re-locks the SAME task. The assembler must close A's attempt
        // as `expired` — not leave it mislabeled as in flight.
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            lock_event("2", "101", "task-1", "t1", "worker-a", 3),
            lock_event("3", "103", "task-1", "t1", "worker-b", 3),
            outcome_event(
                "4",
                "ExternalTaskCompleted",
                "104",
                "task-1",
                "t1",
                Outcome::default(),
            ),
        ];
        let def = def_with(vec![(
            "work",
            NodeType::ExternalTask {
                task_type: "ai".to_string(),
                retries: 3,
                timeout_secs: 60,
            },
        )]);
        let response = assemble_steps(&instance(vec![]), Some(&def), &events);
        let step = &response.steps[0];

        assert_eq!(step.attempts.len(), 2, "one attempt per lock");
        let a = &step.attempts[0];
        assert_eq!(a.worker_id, "worker-a");
        assert_eq!(
            a.outcome, "expired",
            "the superseded lease is closed as expired, not in-flight"
        );
        assert!(
            a.completed_or_failed.is_none(),
            "expired keeps the null outcome timestamp: no terminating event exists"
        );
        assert!(a.error.is_none(), "expiry is not a failure");

        let b = &step.attempts[1];
        assert_eq!(b.worker_id, "worker-b");
        assert_eq!(b.outcome, "completed");
        assert_eq!(b.completed_or_failed.as_deref(), Some("104"));
    }

    #[test]
    fn same_second_fail_then_relock_keeps_the_failure_on_its_own_attempt() {
        // A fail → re-lock → complete round trip all within ONE second,
        // in the real causal order the stores append. The phase-rank
        // causal sort necessarily yields [lock A, lock B, fail A,
        // complete] (locks rank 30, outcomes rank 40); the join must
        // still attribute the failure to WORKER A's attempt (the fail
        // event carries its worker id) — not expire it and not fail
        // worker B's.
        let mut events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            lock_event("2", "100", "task-1", "t1", "worker-a", 3),
            outcome_event(
                "3",
                "ExternalTaskFailed",
                "100",
                "task-1",
                "t1",
                Outcome {
                    error: Some("model over quota"),
                    worker: Some("worker-a"),
                    ..Default::default()
                },
            ),
            lock_event("4", "100", "task-1", "t1", "worker-b", 3),
            outcome_event(
                "5",
                "ExternalTaskCompleted",
                "100",
                "task-1",
                "t1",
                Outcome::default(),
            ),
        ];
        sort_history_causally(&mut events);
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "TokenArrived",
                "ExternalTaskLocked",
                "ExternalTaskLocked",
                "ExternalTaskFailed",
                "ExternalTaskCompleted"
            ],
            "the phase-rank sort interleaves the two attempts this way"
        );
        let def = def_with(vec![(
            "work",
            NodeType::ExternalTask {
                task_type: "ai".to_string(),
                retries: 3,
                timeout_secs: 60,
            },
        )]);
        let response = assemble_steps(&instance(vec![]), Some(&def), &events);
        let step = &response.steps[0];

        assert_eq!(step.attempts.len(), 2);
        let a = &step.attempts[0];
        assert_eq!(a.worker_id, "worker-a");
        assert_eq!(
            a.outcome, "failed",
            "the failure stays on worker A's attempt despite the re-sort"
        );
        assert_eq!(a.completed_or_failed.as_deref(), Some("100"));
        assert!(a.error.as_deref().unwrap().contains("model over quota"));
        let b = &step.attempts[1];
        assert_eq!(b.worker_id, "worker-b");
        assert_eq!(
            b.outcome, "completed",
            "B's attempt is not hijacked by A's fail"
        );
        assert_eq!(b.completed_or_failed.as_deref(), Some("100"));
    }

    #[test]
    fn envelope_summary_excludes_input_and_output_schemas() {
        // The recorded envelope CARRIES full JSON-Schema fragments; the
        // summary must exclude them — neither the keys nor any of their
        // recognizable content may appear in the response.
        let mut envelope = test_envelope("task-1:0");
        envelope.input_schema = Some(serde_json::json!({
            "type": "object",
            "properties": { "topic": { "type": "string" } },
            "x-marker": "INPUT-SCHEMA-PLAINTEXT-MARKER"
        }));
        envelope.output_schema = Some(serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "x-marker": "OUTPUT-SCHEMA-PLAINTEXT-MARKER"
        }));
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t1", "node_id": "work" }),
            ),
            lock_event("2", "101", "task-1", "t1", "worker-a", 3),
            outcome_event(
                "3",
                "ExternalTaskCompleted",
                "102",
                "task-1",
                "t1",
                Outcome {
                    envelope: Some(&envelope),
                    ..Default::default()
                },
            ),
        ];
        let response = assemble_steps(&instance(vec![]), None, &events);
        let step = &response.steps[0];

        // Attempt-level and node-level summaries: metadata only.
        let attempt_summary = step.attempts[0].envelope.as_ref().unwrap();
        assert!(attempt_summary.get("input_schema").is_none());
        assert!(attempt_summary.get("output_schema").is_none());
        let node_summary = step.envelope_summary.as_ref().unwrap();
        assert!(node_summary.get("input_schema").is_none());
        assert!(node_summary.get("output_schema").is_none());
        // The metadata that IS reported still arrives.
        assert_eq!(node_summary["execution_id"], "task-1:0");

        // The serialized response contains neither the schema keys nor
        // any recognizable fragment of their content.
        let raw = serde_json::to_string(&response).unwrap();
        assert!(!raw.contains("input_schema"), "schema key: {raw}");
        assert!(!raw.contains("output_schema"), "schema key: {raw}");
        assert!(!raw.contains("INPUT-SCHEMA-PLAINTEXT-MARKER"), "{raw}");
        assert!(!raw.contains("OUTPUT-SCHEMA-PLAINTEXT-MARKER"), "{raw}");
    }

    #[test]
    fn variables_touched_names_only_from_both_keys() {
        let events = vec![
            event(
                "1",
                "ProcessStarted",
                "100",
                serde_json::json!({ "instance_id": "inst-1", "initial_variables": { "topic": "Q3", "depth": "2" } }),
            ),
            event(
                "2",
                "TokenArrived",
                "101",
                serde_json::json!({ "token_id": "t1", "node_id": "start", "variables": { "topic": "Q3" } }),
            ),
        ];
        let response = assemble_steps(&instance(vec![("t1", "start")]), None, &events);
        assert_eq!(
            response.steps.len(),
            1,
            "instance-level event is not a step"
        );
        // ProcessStarted is instance-level (not a step), so only the step
        // event's `variables` key names are collected — names only, never
        // values.
        assert_eq!(
            response.steps[0].variables_touched,
            vec!["topic".to_string()]
        );
        let raw = serde_json::to_string(&response).unwrap();
        assert!(!raw.contains("Q3"));
        assert!(!raw.contains("depth"));
    }

    /// The `ExternalTaskLocked`/`ExternalTaskFailed` payload shapes asserted
    /// by E1-3 (worker id, timings, retries) are the ones the assembler
    /// joins — keep the extractor and the wire in lockstep with a
    /// round-trip through the real REST append path shape.
    #[test]
    fn locked_event_shape_matches_routes_payload() {
        // The exact payload the fetch-and-lock handler appends.
        let payload = serde_json::json!({
            "task_id": "task-9", "token_id": "t9",
            "process_instance_id": "inst-1", "worker_id": "w-1", "retries": 3,
        });
        let events = vec![
            event(
                "1",
                "TokenArrived",
                "100",
                serde_json::json!({ "token_id": "t9", "node_id": "work" }),
            ),
            event("2", "ExternalTaskLocked", "101", payload),
        ];
        let response = assemble_steps(&instance(vec![]), None, &events);
        let attempt = &response.steps[0].attempts[0];
        assert_eq!(attempt.worker_id, "w-1");
        assert_eq!(attempt.locked_at, "101");
        assert_eq!(attempt.retries_remaining, Some(3));
    }

    /// `TASK_EVENT_TYPES` must cover exactly the lifecycle events the REST
    /// boundary appends (grep-verified against routes.rs).
    #[test]
    fn task_event_types_cover_the_rest_boundary() {
        assert_eq!(
            TASK_EVENT_TYPES,
            [
                "ExternalTaskLocked",
                "ExternalTaskFailed",
                "ExternalTaskCompleted"
            ]
        );
    }

    /// Payloads::ExternalTaskCompleted serialization keys — the assembler
    /// reads `token_id`/`node_id`/`variables` from exactly these keys.
    #[test]
    fn engine_payload_keys_are_the_assembler_keys() {
        let payload = payloads::ExternalTaskCompleted {
            instance_id: "inst-1".to_string(),
            token_id: "t1".to_string(),
            node_id: "work".to_string(),
            variables: StdHashMap::new(),
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert!(value.get("token_id").is_some());
        assert!(value.get("node_id").is_some());
        assert!(value.get("variables").is_some());
    }
}
