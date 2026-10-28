//! Public DSL document types (v1).
//!
//! These types mirror the restricted node set of ADR-006 and lower
//! field-by-field onto the BPMN intermediate model. JSON field names are the
//! Rust field names in snake_case, except `node_type`/`field_type` which are
//! serialized as `type`, and node-type values which are camelCase
//! (`serviceTask`, `timerIntermediateCatch`, ...).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Restricted node-type set of DSL v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DslNodeType {
    /// Start event: creates the initial token. Exactly one per process.
    Start,
    /// End event: consumes the arriving token. At least one per process.
    End,
    /// Service task: pull-based external task executed by a worker.
    /// Requires `task_type`; optional `retries` (default 3) and
    /// `timeout_secs` (default 60), matching the BPMN parser defaults.
    ServiceTask,
    /// User task: token waits for external completion; optional form.
    UserTask,
    /// XOR gateway: routes to exactly one outgoing edge based on conditions.
    ExclusiveGateway,
    /// AND gateway: forks (1 in, 2+ out) or joins (2+ in, 1 out).
    ParallelGateway,
    /// Timer intermediate catch event: token waits until the timer fires.
    /// Requires `timer.duration` (ISO 8601 duration such as `PT30S`, `PT5M`,
    /// `PT1H`; the runtime parses `PT{N}S`, `PT{N}M`, `PT{N}H` and falls back
    /// to 60 seconds for unsupported formats).
    TimerIntermediateCatch,
}

impl std::fmt::Display for DslNodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DslNodeType::Start => "start",
            DslNodeType::End => "end",
            DslNodeType::ServiceTask => "serviceTask",
            DslNodeType::UserTask => "userTask",
            DslNodeType::ExclusiveGateway => "exclusiveGateway",
            DslNodeType::ParallelGateway => "parallelGateway",
            DslNodeType::TimerIntermediateCatch => "timerIntermediateCatch",
        };
        f.write_str(s)
    }
}

/// Timer definition for `timerIntermediateCatch` nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslTimer {
    /// ISO 8601 duration passed through unchanged to the engine, e.g.
    /// `"PT30S"`, `"PT5M"`, `"PT1H"`. The runtime supports `PT{N}S`,
    /// `PT{N}M`, `PT{N}H`; other values fall back to 60 seconds.
    pub duration: String,
}

/// Data type of a user-task form field. Same set as the engine's
/// `FormFieldType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DslFormFieldType {
    /// Free-text string field.
    String,
    /// Numeric field (integer or decimal).
    Number,
    /// Boolean toggle field.
    Boolean,
    /// Enumeration field with predefined options.
    Enum,
}

/// A single field in a user-task form. Lowers to the engine's `FormField`:
/// `label` defaults to `id`, `type` defaults to `string`, `required` defaults
/// to `false` (same defaults as the BPMN parser).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslFormField {
    /// Unique field identifier within the form. Required.
    pub id: String,
    /// Human-readable label. Defaults to `id` when absent.
    pub label: Option<String>,
    /// Field data type. Defaults to `"string"`.
    #[serde(rename = "type")]
    pub field_type: Option<DslFormFieldType>,
    /// Whether the field must be filled before submission. Defaults to `false`.
    pub required: Option<bool>,
    /// Default value (if not required).
    pub default_value: Option<String>,
    /// Predefined options for `"enum"` fields.
    pub options: Option<Vec<String>>,
}

/// A flow node in the restricted v1 node set. Fields other than `id`, `name`
/// and `type` are only valid for specific node types:
/// `task_type`/`retries`/`timeout_secs` for `serviceTask`, `form_key`/
/// `form_fields` for `userTask`, and `timer` for `timerIntermediateCatch`.
/// The compiler rejects fields set on a node type they do not belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslNode {
    /// Unique non-empty node id. Referenced by edges.
    pub id: String,
    /// Optional human-readable node name.
    pub name: Option<String>,
    /// Node type (restricted v1 set).
    #[serde(rename = "type")]
    pub node_type: DslNodeType,
    /// `serviceTask` only (required): worker topic workers subscribe to.
    pub task_type: Option<String>,
    /// `serviceTask` only: retries before dead-letter. Defaults to 3
    /// (BPMN parser default).
    pub retries: Option<i32>,
    /// `serviceTask` only: lock timeout in seconds. Defaults to 60
    /// (BPMN parser default).
    pub timeout_secs: Option<u64>,
    /// `userTask` only: form key identifier for the task UI.
    pub form_key: Option<String>,
    /// `userTask` only: form field definitions.
    pub form_fields: Option<Vec<DslFormField>>,
    /// `timerIntermediateCatch` only (required): timer definition.
    pub timer: Option<DslTimer>,
}

/// Gateway edge condition. `variableEq` compares a variable for exact string
/// equality (lowered to the canonical expression `${variable == "value"}`);
/// `expression` is an EL expression (supports `==`, `!=`, `>`, `<`, `>=`,
/// `<=`, `and`, `or`), passed through verbatim like a BPMN
/// `conditionExpression`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum DslCondition {
    /// Exact variable match: `variables[variable] == value`.
    VariableEq {
        /// Variable name to compare. Must be non-empty and must not contain
        /// double quotes, `=`, or whitespace (use `expression` otherwise).
        variable: String,
        /// Expected value. Must not contain double quotes (use `expression`
        /// otherwise).
        value: String,
    },
    /// Free-form EL expression, e.g. `"amount > 100"`. May optionally be
    /// wrapped in `${...}` like BPMN; the wrapper is stripped by the shared
    /// compiler.
    Expression {
        /// The expression text. Must not be empty.
        expression: String,
    },
}

/// A directed edge (sequence flow) between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslEdge {
    /// Optional edge id. Auto-generated (`edge_<index>`) when absent; must be
    /// unique across all edges when provided.
    pub id: Option<String>,
    /// Source node id. Must reference an existing node.
    pub source: String,
    /// Target node id. Must reference an existing node.
    pub target: String,
    /// Optional gateway condition. Mutually exclusive with `default: true`.
    pub condition: Option<DslCondition>,
    /// Marks this edge as the default flow of its source gateway (same effect
    /// as a BPMN `exclusiveGateway default` attribute pointing at the flow).
    /// Mutually exclusive with `condition`.
    pub default: Option<bool>,
}

/// Process metadata. `id` becomes the compiled definition id (same behavior
/// as the BPMN process id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DslProcess {
    /// Process definition id. Non-empty; becomes the definition id.
    pub id: String,
    /// Optional process name.
    pub name: Option<String>,
}

/// BPM DSL v1 document: a restricted declarative process definition.
///
/// Node types: `start`, `end`, `serviceTask`, `userTask`, `exclusiveGateway`,
/// `parallelGateway`, `timerIntermediateCatch`. Boundary events, call
/// activities, subprocesses, message/signal events and terminate ends are
/// not part of v1. Unknown fields are rejected (`deny_unknown_fields`), so
/// typos produce precise field-level diagnostics. `schema_version` must be
/// `1`; later versions gate future node sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "BPM DSL v1")]
pub struct DslDocument {
    /// Document format version. Must be `1` for DSL v1.
    pub schema_version: u32,
    /// Process metadata.
    pub process: DslProcess,
    /// Flow nodes. Must not be empty.
    pub nodes: Vec<DslNode>,
    /// Sequence flows between nodes. Each edge references node ids.
    pub edges: Vec<DslEdge>,
}
