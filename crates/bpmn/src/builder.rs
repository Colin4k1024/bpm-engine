//! Typed builder for [`bpm_engine_core::ProcessDefinition`] (issue #21, E4-3).
//!
//! [`ProcessBuilder`] gives embedded Rust developers an ergonomic, validated
//! entry point into the engine without hand-writing BPMN 2.0 XML or DSL JSON
//! strings. It follows the same pipeline as the other two entry points
//! (ADR-006): the builder assembles the shared [`BpmnProcess`] intermediate
//! model — exactly what the XML parser produces — and hands it to
//! [`crate::compile`] **verbatim**, so semantic validation (exactly one start
//! event, at least one end event, orphan nodes, gateway shapes, dead ends,
//! dangling references) and execution semantics are identical to the XML and
//! DSL entries. There is no separate validation code path: [`ProcessBuilder::build`]
//! returns the shared compiler's errors unchanged.
//!
//! # Scope
//!
//! The builder covers the BPMN-expressible subset that all three entries
//! share: external (service) tasks, user tasks, exclusive/parallel gateways
//! and timer intermediate catch events. The core [`bpm_engine_core::NodeType`]
//! also has a `ServiceTask(fn(&mut ProcessInstance))` variant for inline
//! closures, which by construction cannot round-trip through BPMN/DSL; such
//! nodes must be added to a `ProcessDefinition` by hand. Boundary events,
//! subprocesses, call activities, message/signal events and terminate ends are
//! likewise out of scope for the builder (use BPMN XML for those).
//!
//! # Fluent style
//!
//! Node and edge methods append to the graph and return the builder so calls
//! chain. Option setters (`.retries()`, `.form_key()`, `.when_variable_eq()`,
//! ...) apply to the **most recently added** node/edge respectively — the same
//! convention as classic BPMN fluent APIs. Calling an option setter when the
//! most recent node has the wrong kind (or before anything was added) panics
//! immediately with a precise message: this is static programmer error, not
//! process validation, so it never reaches [`build`](ProcessBuilder::build).
//!
//! # Example
//!
//! ```
//! use bpm_engine_bpmn::builder::ProcessBuilder;
//!
//! let def = ProcessBuilder::new("order")
//!     .start_node("start")
//!     .external_task("payment", "payment-topic")
//!         .retries(5)
//!         .timeout_secs(120)
//!     .exclusive_gateway("decide")
//!     .end_node("end-paid")
//!     .end_node("end-skipped")
//!     .edge("start", "payment")
//!     .edge("payment", "decide")
//!     .edge("decide", "end-paid").when_variable_eq("paid", "true")
//!     .edge("decide", "end-skipped").is_default()
//!     .build()
//!     .unwrap();
//!
//! assert_eq!(def.id, "order");
//! assert_eq!(def.start, "start");
//! assert_eq!(def.nodes.len(), 5);
//! ```

use std::collections::HashMap;

use bpm_engine_core::{FormField, FormFieldType, ProcessDefinition};

use crate::errors::{CompileError, CompileErrors};
use crate::model::{BpmnFlowNode, BpmnProcess, BpmnSequenceFlow, TimerType};
use crate::parser::wire_flows;

/// Default `retries` for external tasks — matches the BPMN parser default.
const DEFAULT_SERVICE_RETRIES: i32 = 3;
/// Default `timeout_secs` for external tasks — matches the BPMN parser value.
const DEFAULT_SERVICE_TIMEOUT_SECS: u64 = 60;

/// Specification of one user-task form field, lowered to
/// [`bpm_engine_core::FormField`] at [`ProcessBuilder::build`] time with the
/// same defaults as the BPMN parser and the JSON DSL: `label` defaults to the
/// field `id`, `field_type` defaults to [`FormFieldType::String`], and empty
/// `options` become `None`.
///
/// Construct with the type helpers ([`FormFieldSpec::string`],
/// [`FormFieldSpec::number`], [`FormFieldSpec::boolean`],
/// [`FormFieldSpec::enum_field`]) and chain
/// [`.label()`](FormFieldSpec::label),
/// [`.required()`](FormFieldSpec::required) and
/// [`.default_value()`](FormFieldSpec::default_value), or use a struct literal
/// with `..Default::default()`:
///
/// ```
/// use bpm_engine_bpmn::builder::FormFieldSpec;
/// use bpm_engine_core::FormFieldType;
///
/// let from_helpers = FormFieldSpec::number("amount").required().default_value("0");
/// let from_literal = FormFieldSpec {
///     id: "amount".to_string(),
///     field_type: FormFieldType::Number,
///     required: true,
///     default_value: Some("0".to_string()),
///     ..Default::default()
/// };
/// assert_eq!(from_helpers, from_literal);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct FormFieldSpec {
    /// Unique field identifier within the form.
    pub id: String,
    /// Human-readable label. Defaults to `id` when `None`.
    pub label: Option<String>,
    /// Field data type. Defaults to [`FormFieldType::String`].
    pub field_type: FormFieldType,
    /// Whether the field must be filled before submission. Defaults to `false`.
    pub required: bool,
    /// Default value (typically used when not required).
    pub default_value: Option<String>,
    /// Predefined options for enum fields. Empty lists become `None`.
    pub options: Option<Vec<String>>,
}

// `FormFieldType` derives `PartialEq` (added in the E4-3 review round), so the
// spec derives it too: a future `FormFieldType` variant cannot silently break
// the equality semantics (the pre-derive hand-rolled `matches!` had exactly
// that failure mode).

impl Default for FormFieldSpec {
    fn default() -> Self {
        FormFieldSpec {
            id: String::new(),
            label: None,
            field_type: FormFieldType::String,
            required: false,
            default_value: None,
            options: None,
        }
    }
}

impl FormFieldSpec {
    /// New free-text string field with the given id.
    pub fn string(id: &str) -> Self {
        FormFieldSpec {
            id: id.to_string(),
            field_type: FormFieldType::String,
            ..Default::default()
        }
    }

    /// New numeric field with the given id.
    pub fn number(id: &str) -> Self {
        FormFieldSpec {
            id: id.to_string(),
            field_type: FormFieldType::Number,
            ..Default::default()
        }
    }

    /// New boolean toggle field with the given id.
    pub fn boolean(id: &str) -> Self {
        FormFieldSpec {
            id: id.to_string(),
            field_type: FormFieldType::Boolean,
            ..Default::default()
        }
    }

    /// New enum field with the given id and the allowed option ids.
    pub fn enum_field(id: &str, options: &[&str]) -> Self {
        FormFieldSpec {
            id: id.to_string(),
            field_type: FormFieldType::Enum,
            options: Some(options.iter().map(|o| o.to_string()).collect()),
            ..Default::default()
        }
    }

    /// Set the human-readable label (builder-style).
    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }

    /// Mark the field as required (builder-style).
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Set the default value (builder-style).
    pub fn default_value(mut self, value: &str) -> Self {
        self.default_value = Some(value.to_string());
        self
    }
}

/// Internal node representation held by [`ProcessBuilder`] until lowering.
#[derive(Debug, Clone)]
enum NodeSpec {
    StartEvent {
        name: Option<String>,
    },
    EndEvent {
        name: Option<String>,
    },
    ExternalTask {
        name: Option<String>,
        task_type: String,
        retries: i32,
        timeout_secs: u64,
    },
    UserTask {
        name: Option<String>,
        form_key: Option<String>,
        form_fields: Option<Vec<FormFieldSpec>>,
    },
    TimerCatch {
        name: Option<String>,
        duration: String,
    },
    ExclusiveGateway {
        name: Option<String>,
    },
    ParallelGateway {
        name: Option<String>,
    },
}

impl NodeSpec {
    /// Human-readable kind name for misuse panic messages.
    fn kind_name(&self) -> &'static str {
        match self {
            NodeSpec::StartEvent { .. } => "a start node",
            NodeSpec::EndEvent { .. } => "an end node",
            NodeSpec::ExternalTask { .. } => "an external_task node",
            NodeSpec::UserTask { .. } => "a user_task node",
            NodeSpec::TimerCatch { .. } => "a timer_catch node",
            NodeSpec::ExclusiveGateway { .. } => "an exclusive_gateway node",
            NodeSpec::ParallelGateway { .. } => "a parallel_gateway node",
        }
    }
}

/// Internal edge representation held by [`ProcessBuilder`] until lowering.
/// `condition` stores the raw expression string (canonical
/// `${variable == "value"}` for `when_variable_eq`, verbatim text for
/// `when_expression`), exactly like the DSL lowering.
#[derive(Debug, Clone)]
struct EdgeSpec {
    source: String,
    target: String,
    condition: Option<String>,
    is_default: bool,
}

/// Fluent builder producing a compiled [`ProcessDefinition`] through the
/// shared BPMN compiler (see the [module docs](self)).
///
/// Node methods (`start_node`, `end_node`, `external_task`, `user_task`,
/// `timer_catch`, `exclusive_gateway`, `parallel_gateway`) and `edge` append
/// to the graph; option setters apply to the most recently added node or edge.
/// See [`build`](Self::build) for the returned errors.
#[derive(Debug, Clone)]
pub struct ProcessBuilder {
    process_id: String,
    process_name: Option<String>,
    nodes: HashMap<String, NodeSpec>,
    edges: Vec<EdgeSpec>,
    last_node: Option<String>,
}

impl ProcessBuilder {
    /// Create a new builder for a process with the given definition id
    /// (becomes the compiled `ProcessDefinition::id`, same as the BPMN
    /// process id).
    pub fn new(process_id: &str) -> Self {
        ProcessBuilder {
            process_id: process_id.to_string(),
            process_name: None,
            nodes: HashMap::new(),
            edges: Vec::new(),
            last_node: None,
        }
    }

    /// Set the process name. Cosmetic only: carried on the shared intermediate
    /// model like the BPMN process `name` attribute and the DSL `process.name`
    /// field, and — like both other entries — it does **not** appear in the
    /// compiled `ProcessDefinition` (the compiled model has no name field).
    pub fn process_name(&mut self, name: &str) -> &mut Self {
        self.process_name = Some(name.to_string());
        self
    }

    /// Add a start event. Exactly one is required (shared compiler check:
    /// `NoStartEvent` / `MultipleStartEvents`).
    pub fn start_node(&mut self, id: &str) -> &mut Self {
        self.add_node(id, NodeSpec::StartEvent { name: None })
    }

    /// Add an end event. At least one is required (shared compiler check:
    /// `NoEndEvent`).
    pub fn end_node(&mut self, id: &str) -> &mut Self {
        self.add_node(id, NodeSpec::EndEvent { name: None })
    }

    /// Add an external (service) task executed by pull-based workers on the
    /// given `task_type` topic. Defaults: `retries = 3`, `timeout_secs = 60`
    /// (identical to the BPMN parser and DSL defaults); override with
    /// [`retries`](Self::retries) and [`timeout_secs`](Self::timeout_secs).
    pub fn external_task(&mut self, id: &str, task_type: &str) -> &mut Self {
        self.add_node(
            id,
            NodeSpec::ExternalTask {
                name: None,
                task_type: task_type.to_string(),
                retries: DEFAULT_SERVICE_RETRIES,
                timeout_secs: DEFAULT_SERVICE_TIMEOUT_SECS,
            },
        )
    }

    /// Add a user task whose token waits for external completion. Optional
    /// form via [`form_key`](Self::form_key) and
    /// [`form_field`](Self::form_field).
    pub fn user_task(&mut self, id: &str) -> &mut Self {
        self.add_node(
            id,
            NodeSpec::UserTask {
                name: None,
                form_key: None,
                form_fields: None,
            },
        )
    }

    /// Add a timer intermediate catch event: the token waits until the timer
    /// fires. `duration` is an ISO 8601 duration such as `"PT1S"`, `"PT5M"`,
    /// `"PT1H"` (the runtime parses `PT{N}S`/`PT{N}M`/`PT{N}H` and falls back
    /// to 60 seconds for unsupported formats), passed through unchanged like
    /// the BPMN `timeDuration` and the DSL `timer.duration`.
    pub fn timer_catch(&mut self, id: &str, duration: &str) -> &mut Self {
        self.add_node(
            id,
            NodeSpec::TimerCatch {
                name: None,
                duration: duration.to_string(),
            },
        )
    }

    /// Add an exclusive (XOR) gateway. Conditionally routed outgoing edges
    /// need one `is_default` edge (shared compiler check:
    /// `ExclusiveGatewayNoDefault`).
    pub fn exclusive_gateway(&mut self, id: &str) -> &mut Self {
        self.add_node(id, NodeSpec::ExclusiveGateway { name: None })
    }

    /// Add a parallel (AND) gateway. Use one gateway as a fork (1 incoming,
    /// 2+ outgoing) and a separate one as a join (2+ incoming, 1 outgoing)
    /// (shared compiler check: `ParallelGatewayInvalidShape`).
    pub fn parallel_gateway(&mut self, id: &str) -> &mut Self {
        self.add_node(id, NodeSpec::ParallelGateway { name: None })
    }

    /// Set the name of the most recently added node. Cosmetic only: carried on
    /// the shared intermediate model like the BPMN node `name` attribute and
    /// the DSL `nodes[].name` field, and — like both other entries — it does
    /// **not** appear in the compiled `ProcessDefinition` (`Node` has no name
    /// field).
    ///
    /// # Panics
    ///
    /// Panics if no node has been added yet.
    pub fn name(&mut self, name: &str) -> &mut Self {
        let (_, spec) = self.last_node_mut("`name`");
        set_node_name(spec, name);
        self
    }

    /// Set the retry count of the most recently added **external task**
    /// (retries before the task moves to the dead-letter queue). Default: 3.
    ///
    /// # Panics
    ///
    /// Panics if the most recent node is not an `external_task` node.
    pub fn retries(&mut self, retries: i32) -> &mut Self {
        let (id, spec) = self.last_node_mut("`retries`");
        match spec {
            NodeSpec::ExternalTask { retries: r, .. } => *r = retries,
            other => panic!(
                "`retries` only applies to external_task nodes, but the most recent node `{id}` is {}",
                other.kind_name()
            ),
        }
        self
    }

    /// Set the lock timeout (seconds) of the most recently added **external
    /// task**. Default: 60.
    ///
    /// # Panics
    ///
    /// Panics if the most recent node is not an `external_task` node.
    pub fn timeout_secs(&mut self, timeout_secs: u64) -> &mut Self {
        let (id, spec) = self.last_node_mut("`timeout_secs`");
        match spec {
            NodeSpec::ExternalTask { timeout_secs: t, .. } => *t = timeout_secs,
            other => panic!(
                "`timeout_secs` only applies to external_task nodes, but the most recent node `{id}` is {}",
                other.kind_name()
            ),
        }
        self
    }

    /// Set the form key of the most recently added **user task** (identifier
    /// the task UI renders).
    ///
    /// # Panics
    ///
    /// Panics if the most recent node is not a `user_task` node.
    pub fn form_key(&mut self, form_key: &str) -> &mut Self {
        let (id, spec) = self.last_node_mut("`form_key`");
        match spec {
            NodeSpec::UserTask { form_key: k, .. } => *k = Some(form_key.to_string()),
            other => panic!(
                "`form_key` only applies to user_task nodes, but the most recent node `{id}` is {}",
                other.kind_name()
            ),
        }
        self
    }

    /// Append a form field to the most recently added **user task**.
    /// See [`FormFieldSpec`] for field defaults.
    ///
    /// # Panics
    ///
    /// Panics if the most recent node is not a `user_task` node.
    pub fn form_field(&mut self, field: FormFieldSpec) -> &mut Self {
        let (id, spec) = self.last_node_mut("`form_field`");
        match spec {
            NodeSpec::UserTask { form_fields, .. } => {
                form_fields.get_or_insert_with(Vec::new).push(field);
            }
            other => panic!(
                "`form_field` only applies to user_task nodes, but the most recent node `{id}` is {}",
                other.kind_name()
            ),
        }
        self
    }

    /// Add a sequence flow between two node ids. Edge ids are generated
    /// deterministically (`flow_<index>`); the compiled definition keys edges
    /// by node id, so they never collide with user-visible identifiers.
    /// Dangling references are rejected by the shared compiler
    /// (`SequenceFlowSourceNotFound` / `SequenceFlowTargetNotFound`).
    ///
    /// Conditions are attached with [`when_variable_eq`](Self::when_variable_eq),
    /// [`when_expression`](Self::when_expression) or [`is_default`](Self::is_default).
    pub fn edge(&mut self, source: &str, target: &str) -> &mut Self {
        self.edges.push(EdgeSpec {
            source: source.to_string(),
            target: target.to_string(),
            condition: None,
            is_default: false,
        });
        self
    }

    /// Route the most recently added edge when the given process variable
    /// equals the given value (exact string comparison). Lowers to the
    /// canonical expression `${variable == "value"}` — the same lowering as
    /// the DSL `variableEq` condition.
    ///
    /// # Panics
    ///
    /// Panics if no edge has been added yet, if `variable` is empty or
    /// contains a double quote, `=` or whitespace, or if `value` contains a
    /// double quote (use [`when_expression`](Self::when_expression) for such
    /// cases). These mirror the DSL's condition-safety rules.
    pub fn when_variable_eq(&mut self, variable: &str, value: &str) -> &mut Self {
        if variable.trim().is_empty() {
            panic!("`when_variable_eq` variable must be a non-empty string");
        }
        if variable.contains('"')
            || variable.contains('=')
            || variable.chars().any(char::is_whitespace)
        {
            panic!(
                "`when_variable_eq` variable must not contain double quotes, `=` or whitespace \
                 (use `when_expression` for advanced variable names): `{variable}`"
            );
        }
        if value.contains('"') {
            panic!(
                "`when_variable_eq` value must not contain double quotes \
                 (use `when_expression` for values with quotes): `{value}`"
            );
        }
        let edge = self.last_edge_mut("`when_variable_eq`");
        edge.condition = Some(format!("${{{} == \"{}\"}}", variable, value));
        edge.is_default = false;
        self
    }

    /// Route the most recently added edge on a free-form EL expression, e.g.
    /// `"amount > 100"`. Supports `==`, `!=`, `>`, `<`, `>=`, `<=`, `and`,
    /// `or`; an optional `${...}` wrapper is stripped by the shared compiler,
    /// exactly like a BPMN `conditionExpression`.
    ///
    /// # Panics
    ///
    /// Panics if no edge has been added yet or if the expression is empty
    /// (after trimming and stripping an optional `${...}` wrapper).
    pub fn when_expression(&mut self, expression: &str) -> &mut Self {
        let trimmed = expression.trim();
        let inner = trimmed
            .strip_prefix("${")
            .and_then(|t| t.strip_suffix('}'))
            .map(str::trim)
            .unwrap_or(trimmed);
        if inner.is_empty() {
            panic!("`when_expression` expression must not be empty");
        }
        let edge = self.last_edge_mut("`when_expression`");
        edge.condition = Some(expression.to_string());
        edge.is_default = false;
        self
    }

    /// Mark the most recently added edge as the default flow of its source
    /// exclusive gateway (taken when no condition matches). Mutually
    /// exclusive with a condition: the most recently set one wins.
    ///
    /// # Panics
    ///
    /// Panics if no edge has been added yet.
    pub fn is_default(&mut self) -> &mut Self {
        let edge = self.last_edge_mut("`is_default`");
        edge.is_default = true;
        edge.condition = None;
        self
    }

    /// Lower the accumulated graph onto the shared [`BpmnProcess`] model and
    /// compile it with [`crate::compile`], returning the compiled
    /// [`ProcessDefinition`] or the shared compiler's errors verbatim
    /// (wrapped in [`CompileError::Compile`] like `parse_and_compile`).
    ///
    /// All semantic validation is the shared compiler's: start/end counts,
    /// orphan nodes, gateway shapes, dead ends and dangling edge references
    /// come back with their stable [`crate::ErrorCode`]s.
    pub fn build(&self) -> Result<ProcessDefinition, CompileError> {
        let model = self.to_bpmn_process();
        match crate::compile(model) {
            Ok(def) => Ok(def),
            Err(errors) => Err(CompileError::Compile(CompileErrors(errors))),
        }
    }

    /// Register a node, rejecting duplicate ids and recording the node as the
    /// target of subsequent node option setters.
    ///
    /// The duplicate check runs BEFORE the insert, so a `catch_unwind` caller
    /// that observes the panic never sees a mutated builder (the duplicate
    /// node is not half-registered).
    fn add_node(&mut self, id: &str, spec: NodeSpec) -> &mut Self {
        if self.nodes.contains_key(id) {
            panic!("duplicate node id `{id}`: node ids must be unique within a process");
        }
        self.nodes.insert(id.to_string(), spec);
        self.last_node = Some(id.to_string());
        self
    }

    /// The most recently added node, or a panic naming the misused setter.
    fn last_node_mut(&mut self, setter: &str) -> (String, &mut NodeSpec) {
        let Some(id) = self.last_node.clone() else {
            panic!("{setter} needs a node to configure, but none has been added yet");
        };
        // `add_node` inserts every node before setting `last_node`, so the
        // entry always exists.
        let spec = self
            .nodes
            .get_mut(&id)
            .expect("last_node always references an inserted node");
        (id, spec)
    }

    /// The most recently added edge, or a panic naming the misused setter.
    fn last_edge_mut(&mut self, setter: &str) -> &mut EdgeSpec {
        self.edges.last_mut().unwrap_or_else(|| {
            panic!("{setter} needs an edge to configure, but none has been added yet")
        })
    }

    /// Lower onto the shared intermediate model (identical shape to what
    /// `bpm_engine_bpmn::parse` produces, including the parser's final
    /// `wire_flows` step).
    fn to_bpmn_process(&self) -> BpmnProcess {
        let flow_nodes: HashMap<String, BpmnFlowNode> = self
            .nodes
            .iter()
            .map(|(id, spec)| (id.clone(), lower_node(id, spec)))
            .collect();

        let sequence_flows: Vec<BpmnSequenceFlow> = self
            .edges
            .iter()
            .enumerate()
            .map(|(i, edge)| BpmnSequenceFlow {
                id: format!("flow_{}", i),
                source_ref: edge.source.clone(),
                target_ref: edge.target.clone(),
                condition_expression: edge.condition.clone(),
                is_default: edge.is_default,
            })
            .collect();

        let mut model = BpmnProcess {
            id: self.process_id.clone(),
            name: self.process_name.clone(),
            flow_nodes,
            sequence_flows,
        };
        // Identical to the parser's final step: attach flow ids to nodes.
        wire_flows(&mut model);
        model
    }
}

/// Set the `name` field on any node spec (every variant carries one).
fn set_node_name(spec: &mut NodeSpec, new_name: &str) {
    match spec {
        NodeSpec::StartEvent { name }
        | NodeSpec::EndEvent { name }
        | NodeSpec::ExternalTask { name, .. }
        | NodeSpec::UserTask { name, .. }
        | NodeSpec::TimerCatch { name, .. }
        | NodeSpec::ExclusiveGateway { name }
        | NodeSpec::ParallelGateway { name } => *name = Some(new_name.to_string()),
    }
}

/// Lower one node spec to the shared model, reproducing the BPMN parser's
/// field mapping (field-by-field, same defaults).
fn lower_node(id: &str, spec: &NodeSpec) -> BpmnFlowNode {
    match spec {
        NodeSpec::StartEvent { name } => BpmnFlowNode::StartEvent {
            id: id.to_string(),
            name: name.clone(),
            incoming: vec![],
            outgoing: vec![],
        },
        NodeSpec::EndEvent { name } => BpmnFlowNode::EndEvent {
            id: id.to_string(),
            name: name.clone(),
            incoming: vec![],
            outgoing: vec![],
        },
        NodeSpec::ExternalTask {
            name,
            task_type,
            retries,
            timeout_secs,
        } => BpmnFlowNode::ServiceTask {
            id: id.to_string(),
            name: name.clone(),
            task_type: task_type.clone(),
            retries: *retries,
            timeout_secs: *timeout_secs,
            incoming: vec![],
            outgoing: vec![],
        },
        NodeSpec::UserTask {
            name,
            form_key,
            form_fields,
        } => BpmnFlowNode::UserTask {
            id: id.to_string(),
            name: name.clone(),
            form_key: form_key.clone(),
            form_fields: form_fields
                .as_ref()
                .map(|fields| fields.iter().map(lower_form_field).collect()),
            incoming: vec![],
            outgoing: vec![],
        },
        NodeSpec::TimerCatch { name, duration } => BpmnFlowNode::TimerIntermediateCatchEvent {
            id: id.to_string(),
            name: name.clone(),
            timer_type: TimerType::TimeDuration(duration.clone()),
            incoming: vec![],
            outgoing: vec![],
        },
        NodeSpec::ExclusiveGateway { name } => BpmnFlowNode::ExclusiveGateway {
            id: id.to_string(),
            name: name.clone(),
            incoming: vec![],
            outgoing: vec![],
        },
        NodeSpec::ParallelGateway { name } => BpmnFlowNode::ParallelGateway {
            id: id.to_string(),
            name: name.clone(),
            incoming: vec![],
            outgoing: vec![],
        },
    }
}

/// Lower a form field spec, reproducing the BPMN parser's and the DSL's
/// defaults: `label` defaults to `id`, empty options become `None`.
fn lower_form_field(spec: &FormFieldSpec) -> FormField {
    FormField {
        id: spec.id.clone(),
        label: spec.label.clone().unwrap_or_else(|| spec.id.clone()),
        field_type: spec.field_type.clone(),
        required: spec.required,
        default_value: spec.default_value.clone(),
        options: match &spec.options {
            Some(options) if !options.is_empty() => Some(options.clone()),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_field_spec_helpers_and_literals_agree() {
        let helpers = FormFieldSpec::enum_field("priority", &["high", "low"])
            .label("Priority")
            .required();
        let literal = FormFieldSpec {
            id: "priority".to_string(),
            label: Some("Priority".to_string()),
            field_type: FormFieldType::Enum,
            required: true,
            default_value: None,
            options: Some(vec!["high".to_string(), "low".to_string()]),
        };
        assert_eq!(helpers, literal);
    }

    #[test]
    fn lower_form_field_applies_bpmn_defaults() {
        // label defaults to id; type default is String; empty options -> None.
        let lowered = lower_form_field(&FormFieldSpec::string("reason"));
        assert_eq!(lowered.label, "reason");
        assert!(matches!(lowered.field_type, FormFieldType::String));
        assert!(!lowered.required);
        assert!(lowered.options.is_none());

        let lowered = lower_form_field(&FormFieldSpec::enum_field("p", &[]));
        assert!(matches!(lowered.field_type, FormFieldType::Enum));
        assert!(lowered.options.is_none());
    }

    #[test]
    fn external_task_defaults_match_bpmn_parser() {
        let mut b = ProcessBuilder::new("p");
        b.external_task("work", "orders");
        let model = b.to_bpmn_process();
        match &model.flow_nodes["work"] {
            BpmnFlowNode::ServiceTask {
                task_type,
                retries,
                timeout_secs,
                ..
            } => {
                assert_eq!(task_type, "orders");
                assert_eq!(*retries, 3);
                assert_eq!(*timeout_secs, 60);
            }
            _ => panic!("expected ServiceTask"),
        }
    }

    #[test]
    fn timer_catch_lowers_time_duration() {
        let mut b = ProcessBuilder::new("p");
        b.timer_catch("wait", "PT1S");
        let model = b.to_bpmn_process();
        match &model.flow_nodes["wait"] {
            BpmnFlowNode::TimerIntermediateCatchEvent { timer_type, .. } => match timer_type {
                TimerType::TimeDuration(d) => assert_eq!(d, "PT1S"),
                _ => panic!("expected TimeDuration"),
            },
            _ => panic!("expected TimerIntermediateCatchEvent"),
        }
    }

    #[test]
    fn edges_generate_deterministic_ids_and_wire() {
        let mut b = ProcessBuilder::new("p");
        b.start_node("s").end_node("e").edge("s", "e");
        let model = b.to_bpmn_process();
        assert_eq!(model.sequence_flows.len(), 1);
        assert_eq!(model.sequence_flows[0].id, "flow_0");
        assert_eq!(model.sequence_flows[0].source_ref, "s");
        assert_eq!(model.sequence_flows[0].target_ref, "e");
        // wire_flows attached the flow id on both sides.
        assert_eq!(model.flow_nodes["s"].outgoing(), &["flow_0".to_string()]);
        assert_eq!(model.flow_nodes["e"].incoming(), &["flow_0".to_string()]);
    }

    #[test]
    fn when_variable_eq_lowers_canonical_expression() {
        let mut b = ProcessBuilder::new("p");
        b.start_node("s")
            .end_node("e")
            .edge("s", "e")
            .when_variable_eq("approved", "true");
        let model = b.to_bpmn_process();
        assert_eq!(
            model.sequence_flows[0].condition_expression.as_deref(),
            Some("${approved == \"true\"}")
        );
        assert!(!model.sequence_flows[0].is_default);
    }

    #[test]
    fn condition_and_default_are_last_write_wins() {
        let mut b = ProcessBuilder::new("p");
        b.start_node("s").end_node("e").edge("s", "e");
        b.when_variable_eq("x", "1");
        b.is_default();
        assert!(b.edges[0].is_default);
        assert!(b.edges[0].condition.is_none());

        b.when_expression("x > 1");
        assert!(!b.edges[0].is_default);
        assert_eq!(b.edges[0].condition.as_deref(), Some("x > 1"));
    }

    #[test]
    #[should_panic(expected = "duplicate node id `s`")]
    fn duplicate_node_id_panics() {
        ProcessBuilder::new("p").start_node("s").start_node("s");
    }

    #[test]
    fn duplicate_node_panic_leaves_builder_unmutated() {
        // The duplicate check runs before the insert, so a `catch_unwind`
        // caller never observes a corrupted builder. (The pre-fix code
        // inserted first — replacing the original registration — and only
        // then panicked, leaving the map holding the duplicate's spec.)
        let mut b = ProcessBuilder::new("p");
        b.start_node("s");
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            b.end_node("s"); // duplicate id, different kind
        }));
        assert!(panicked.is_err());
        assert!(matches!(
            b.nodes.get("s"),
            Some(NodeSpec::StartEvent { .. })
        ));
        assert_eq!(b.nodes.len(), 1);
        // The builder remains usable after the caught panic.
        b.end_node("e").edge("s", "e");
        assert!(b.build().is_ok());
    }

    #[test]
    #[should_panic(expected = "`retries` only applies to external_task nodes")]
    fn retries_on_wrong_node_kind_panics() {
        ProcessBuilder::new("p")
            .user_task("u")
            .retries(2)
            .end_node("e");
    }

    #[test]
    #[should_panic(expected = "`form_key` only applies to user_task nodes")]
    fn form_key_on_wrong_node_kind_panics() {
        ProcessBuilder::new("p")
            .end_node("e")
            .form_key("k")
            .start_node("s");
    }

    #[test]
    #[should_panic(expected = "`when_variable_eq` needs an edge")]
    fn condition_before_edge_panics() {
        ProcessBuilder::new("p")
            .start_node("s")
            .when_variable_eq("x", "1");
    }

    #[test]
    #[should_panic(expected = "`name` needs a node")]
    fn name_before_any_node_panics() {
        ProcessBuilder::new("p").name("x");
    }

    #[test]
    #[should_panic(expected = "`when_variable_eq` variable must not contain")]
    fn variable_eq_with_quote_panics() {
        let mut b = ProcessBuilder::new("p");
        b.start_node("s").end_node("e").edge("s", "e");
        b.when_variable_eq("bad variable", "1");
    }

    #[test]
    #[should_panic(expected = "`when_expression` expression must not be empty")]
    fn empty_expression_panics() {
        let mut b = ProcessBuilder::new("p");
        b.start_node("s").end_node("e").edge("s", "e");
        b.when_expression("  ");
    }
}
