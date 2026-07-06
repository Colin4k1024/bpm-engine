use std::collections::HashMap;

/// Node identifier within a process definition (static lifetime from BPMN compilation).
pub type NodeId = &'static str;

/// Data type of a user task form field.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormFieldType {
    /// Free-text string field.
    String,
    /// Numeric field (integer or decimal).
    Number,
    /// Boolean toggle field.
    Boolean,
    /// Enumeration field with predefined options.
    Enum,
}

/// A single field in a user task form definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FormField {
    /// Unique field identifier within the form.
    pub id: String,
    /// Human-readable label displayed to the user.
    pub label: String,
    /// Data type of the field.
    #[serde(rename = "type")]
    pub field_type: FormFieldType,
    /// Whether the field must be filled before submission.
    pub required: bool,
    /// Default value for the field (if not required).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// Available options for enum fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

/// Metadata for a boundary event attached to a host activity.
#[derive(Debug, Clone)]
pub struct BoundaryEventDef {
    /// Node ID of the boundary event (used as target for outgoing edges).
    pub node_id: NodeId,
    /// Whether this boundary event interrupts the host activity.
    pub is_interrupting: bool,
    /// The single outgoing target node when the boundary event fires.
    pub target_node_id: NodeId,
}

/// Condition evaluated at exclusive gateways to select the outgoing path.
#[derive(Debug, Clone)]
pub enum EdgeCondition {
    /// Exact variable match: `variables[key] == value`.
    VariableEq {
        /// Variable name to compare.
        key: String,
        /// Expected value.
        value: String,
    },
    /// Free-form expression (evaluated by the EL engine).
    Expression(String),
    /// Default path taken when no other condition matches.
    Default,
}

/// A directed edge from one BPMN node to another, optionally guarded by a condition.
#[derive(Debug, Clone)]
pub struct OutgoingEdge {
    /// Target node this edge connects to.
    pub target: NodeId,
    /// Optional guard condition (exclusive gateways).
    pub condition: Option<EdgeCondition>,
}

/// The semantic type of a BPMN node, determining how the engine executes it.
#[derive(Debug, Clone)]
pub enum NodeType {
    /// BPMN Start Event — creates the initial token.
    Start,
    /// BPMN End Event — consumes and completes the arriving token.
    End,
    /// Inline service task: synchronous function executed in-process.
    ServiceTask(fn(&mut super::instance::ProcessInstance)),
    /// Human task — token waits until external completion signal.
    UserTask {
        /// Optional form key identifier for the task UI.
        form_key: Option<String>,
        /// Form field definitions for the task.
        form_fields: Option<Vec<FormField>>,
    },
    /// Pull-based external task: worker fetches, locks, completes/fails.
    ExternalTask {
        /// Task type (topic) that workers subscribe to.
        task_type: String,
        /// Number of retries before the task moves to dead letter queue.
        retries: i32,
        /// Lock timeout in seconds.
        timeout_secs: u64,
    },
    /// XOR gateway — routes to exactly one outgoing edge based on conditions.
    ExclusiveGateway,
    /// AND split — creates one token per outgoing edge.
    ParallelFork,
    /// AND join — waits until all expected branches arrive before continuing.
    ParallelJoin {
        /// Number of branches that must arrive before the join completes.
        expected: usize,
    },
    /// Timer intermediate catch event — token waits until a timer fires.
    TimerIntermediateCatch {
        /// Timer definition: ISO 8601 duration (e.g. "PT1H"), absolute date, or cycle.
        timer_definition: String,
    },
    /// Timer boundary event — fires a timer attached to a host activity.
    BoundaryTimer {
        /// ISO 8601 duration before the boundary event fires.
        duration: String,
        /// Whether firing this event interrupts the host activity.
        is_interrupting: bool,
    },
    /// Error boundary event — fires when the host activity fails.
    BoundaryError {
        /// Optional error code to match. `None` catches any error.
        error_code: Option<String>,
        /// Whether firing this event interrupts the host activity.
        is_interrupting: bool,
    },
    /// Call activity — invokes an external process definition by key.
    CallActivity {
        /// Process definition key to invoke (resolved at runtime).
        called_process_key: String,
    },
    /// Intermediate catch event that waits for a named message.
    MessageIntermediateCatch {
        /// Message name to wait for.
        message_name: String,
    },
    /// Intermediate throw event that sends a named message.
    MessageIntermediateThrow {
        /// Message name to send.
        message_name: String,
    },
    /// Intermediate throw event that fires a named signal (global broadcast).
    SignalIntermediateThrow {
        /// Signal name to fire.
        signal_name: String,
    },
    /// Intermediate catch event that waits for a named signal.
    SignalIntermediateCatch {
        /// Signal name to subscribe to.
        signal_name: String,
    },
    /// End event that terminates all active tokens in the process instance.
    TerminateEnd,
}

/// A node in the process graph with its type and outgoing connections.
#[derive(Debug, Clone)]
pub struct Node {
    /// Unique node identifier within the process definition.
    pub id: NodeId,
    /// Semantic type determining execution behavior.
    pub node_type: NodeType,
    /// Outgoing edges connecting to successor nodes.
    pub outgoing_edges: Vec<OutgoingEdge>,
}

/// Compiled BPMN process definition ready for execution.
///
/// Created by [`bpm_engine_bpmn::parse_and_compile`] from BPMN 2.0 XML.
/// Contains the full node graph with static-lifetime string references
/// (allocated via `Box::leak` during compilation for zero-copy execution).
#[derive(Debug, Clone)]
pub struct ProcessDefinition {
    /// Process definition identifier (from BPMN `process id`).
    pub id: &'static str,
    /// All nodes in the process graph, keyed by node ID.
    pub nodes: HashMap<NodeId, Node>,
    /// Entry point node for new process instances.
    pub start: NodeId,
    /// Boundary events indexed by their host activity node ID.
    pub boundary_events: HashMap<NodeId, Vec<BoundaryEventDef>>,
}
