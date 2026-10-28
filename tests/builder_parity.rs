//! Three-way parity test (issue #21 / E4-3): the typed Rust builder, the JSON
//! DSL and BPMN 2.0 XML compile the same flow to structurally identical
//! `ProcessDefinition`s — the ADR-006 acceptance bar extended to the third
//! entry point.
//!
//! The comparison normalizes the compiled definitions (the `&'static str` ids
//! are leaked per compilation, so equality goes through owned strings and the
//! `Debug` form of `NodeType`/`EdgeCondition`), reusing the approach of
//! `crates/dsl/tests/parity_bpmn.rs`. This test lives in the workspace root
//! because the `bpmn` crate does not depend on `bpm-engine-dsl`.

use std::collections::BTreeMap;

use bpm_engine::bpm_engine_bpmn::builder::{FormFieldSpec, ProcessBuilder};
use bpm_engine::bpm_engine_bpmn::parse_and_compile;
use bpm_engine::bpm_engine_dsl::compile as dsl_compile;
use bpm_engine_core::ProcessDefinition;

/// A rich document covering every shared node type, both condition kinds
/// (`variableEq` and `expression`), the default flag, form-field defaults,
/// and gateway fork/join shapes (same document as
/// `crates/dsl/tests/parity_bpmn.rs`).
const DSL_DOC: &str = r#"{
  "schema_version": 1,
  "process": { "id": "parity-flow", "name": "Parity Flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    {
      "id": "payment",
      "name": "Payment",
      "type": "serviceTask",
      "task_type": "payment",
      "retries": 5,
      "timeout_secs": 60
    },
    { "id": "fork", "type": "parallelGateway" },
    {
      "id": "approval",
      "type": "userTask",
      "form_key": "approval-form",
      "form_fields": [
        { "id": "amount", "type": "number", "required": true, "default_value": "0" },
        { "id": "reason", "label": "Reason", "type": "string" },
        { "id": "priority", "type": "enum", "options": ["high", "low"] }
      ]
    },
    { "id": "wait", "type": "timerIntermediateCatch", "timer": { "duration": "PT1H" } },
    { "id": "join", "type": "parallelGateway" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end-yes", "type": "end" },
    { "id": "end-big", "type": "end" },
    { "id": "end-no", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "payment" },
    { "source": "payment", "target": "fork" },
    { "source": "fork", "target": "approval" },
    { "source": "fork", "target": "wait" },
    { "source": "approval", "target": "join" },
    { "source": "wait", "target": "join" },
    { "source": "join", "target": "xor" },
    {
      "source": "xor",
      "target": "end-yes",
      "condition": { "kind": "variableEq", "variable": "amount", "value": "100" }
    },
    {
      "source": "xor",
      "target": "end-big",
      "condition": { "kind": "expression", "expression": "amount > 100" }
    },
    { "source": "xor", "target": "end-no", "default": true }
  ]
}"#;

/// The BPMN equivalent, field by field: same ids, same flow order, camunda
/// extension attributes for task type / retries / form, timer duration,
/// gateway `default` attribute, `${amount == "100"}` condition text
/// (what the DSL's variableEq and the builder's `when_variable_eq` lower to),
/// and `${amount > 100}` (the free-form expression kind — what the DSL's
/// `expression` condition carries verbatim and the builder's
/// `when_expression` passes through).
const BPMN_EQUIVALENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:camunda="http://camunda.org/schema/1.0/bpmn">
  <process id="parity-flow" name="Parity Flow" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="payment" name="Payment" camunda:retries="5">
      <extensionElements>
        <taskDefinition type="payment"/>
      </extensionElements>
    </serviceTask>
    <parallelGateway id="fork"/>
    <userTask id="approval" camunda:formKey="approval-form">
      <extensionElements>
        <camunda:formData>
          <camunda:formField id="amount" type="number" defaultValue="0">
            <camunda:validation>
              <camunda:constraint name="required"/>
            </camunda:validation>
          </camunda:formField>
          <camunda:formField id="reason" label="Reason" type="string"/>
          <camunda:formField id="priority" type="enum">
            <camunda:value id="high"/>
            <camunda:value id="low"/>
          </camunda:formField>
        </camunda:formData>
      </extensionElements>
    </userTask>
    <intermediateCatchEvent id="wait">
      <timerEventDefinition>
        <timeDuration>PT1H</timeDuration>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <parallelGateway id="join"/>
    <exclusiveGateway id="xor" default="flow9"/>
    <endEvent id="end-yes"/>
    <endEvent id="end-big"/>
    <endEvent id="end-no"/>
    <sequenceFlow id="flow0" sourceRef="start" targetRef="payment"/>
    <sequenceFlow id="flow1" sourceRef="payment" targetRef="fork"/>
    <sequenceFlow id="flow2" sourceRef="fork" targetRef="approval"/>
    <sequenceFlow id="flow3" sourceRef="fork" targetRef="wait"/>
    <sequenceFlow id="flow4" sourceRef="approval" targetRef="join"/>
    <sequenceFlow id="flow5" sourceRef="wait" targetRef="join"/>
    <sequenceFlow id="flow6" sourceRef="join" targetRef="xor"/>
    <sequenceFlow id="flow7" sourceRef="xor" targetRef="end-yes">
      <conditionExpression>${amount == "100"}</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="flow8" sourceRef="xor" targetRef="end-big">
      <conditionExpression>${amount > 100}</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="flow9" sourceRef="xor" targetRef="end-no"/>
  </process>
</definitions>"#;

/// The builder equivalent: same node ids, same edge order, same options.
/// `timeout_secs` relies on the builder default (60), matching both the DSL
/// document's explicit value and the BPMN parser's hardcoded default. The
/// `amount > 100` edge keeps its `${...}` wrapper (`when_expression` passes
/// the text through verbatim), which the shared compiler strips to the same
/// `EdgeCondition::Expression("amount > 100")` the DSL's bare expression
/// produces.
fn build_with_builder() -> ProcessDefinition {
    ProcessBuilder::new("parity-flow")
        .process_name("Parity Flow")
        .start_node("start")
        .external_task("payment", "payment")
        .name("Payment")
        .retries(5)
        .parallel_gateway("fork")
        .user_task("approval")
        .form_key("approval-form")
        .form_field(
            FormFieldSpec::number("amount")
                .required()
                .default_value("0"),
        )
        .form_field(FormFieldSpec::string("reason").label("Reason"))
        .form_field(FormFieldSpec::enum_field("priority", &["high", "low"]))
        .timer_catch("wait", "PT1H")
        .parallel_gateway("join")
        .exclusive_gateway("xor")
        .end_node("end-yes")
        .end_node("end-big")
        .end_node("end-no")
        .edge("start", "payment")
        .edge("payment", "fork")
        .edge("fork", "approval")
        .edge("fork", "wait")
        .edge("approval", "join")
        .edge("wait", "join")
        .edge("join", "xor")
        .edge("xor", "end-yes")
        .when_variable_eq("amount", "100")
        .edge("xor", "end-big")
        .when_expression("${amount > 100}")
        .edge("xor", "end-no")
        .is_default()
        .build()
        .expect("builder flow must compile")
}

/// One normalized outgoing edge: (target node id, condition debug form).
type NormEdge = (String, Option<String>);

/// One normalized node: (node type debug form, outgoing edges in wire order).
type NormNode = (String, Vec<NormEdge>);

/// Normalized, comparable form of a compiled definition.
#[derive(Debug, PartialEq)]
struct Normalized {
    id: String,
    start: String,
    nodes: BTreeMap<String, NormNode>,
}

fn normalize(def: &ProcessDefinition) -> Normalized {
    let mut nodes = BTreeMap::new();
    for (id, node) in &def.nodes {
        let edges: Vec<(String, Option<String>)> = node
            .outgoing_edges
            .iter()
            .map(|e| {
                (
                    e.target.to_string(),
                    e.condition.as_ref().map(|c| format!("{:?}", c)),
                )
            })
            .collect();
        nodes.insert(id.to_string(), (format!("{:?}", node.node_type), edges));
    }
    Normalized {
        id: def.id.to_string(),
        start: def.start.to_string(),
        nodes,
    }
}

#[test]
fn builder_dsl_and_bpmn_compile_to_identical_definitions() {
    let from_builder = normalize(&build_with_builder());
    let from_dsl = normalize(&dsl_compile(DSL_DOC).expect("DSL document must compile"));
    let from_bpmn = normalize(&parse_and_compile(BPMN_EQUIVALENT).expect("BPMN must compile"));

    assert_eq!(from_builder.id, from_dsl.id);
    assert_eq!(from_builder.id, from_bpmn.id);
    assert_eq!(from_builder.start, from_dsl.start);
    assert_eq!(from_builder.start, from_bpmn.start);

    assert_eq!(
        from_builder.nodes, from_dsl.nodes,
        "builder and DSL must compile identically:\n  builder: {:#?}\n  dsl:     {:#?}",
        from_builder.nodes, from_dsl.nodes
    );
    assert_eq!(
        from_builder.nodes, from_bpmn.nodes,
        "builder and BPMN must compile identically:\n  builder: {:#?}\n  bpmn:    {:#?}",
        from_builder.nodes, from_bpmn.nodes
    );
}

#[test]
fn minimal_builder_dsl_and_bpmn_compile_to_identical_definitions() {
    let from_builder = normalize(
        &ProcessBuilder::new("minimal")
            .start_node("start")
            .end_node("end")
            .edge("start", "end")
            .build()
            .unwrap(),
    );
    let from_dsl = normalize(
        &dsl_compile(
            r#"{
  "schema_version": 1,
  "process": { "id": "minimal" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ],
  "edges": [ { "source": "start", "target": "end" } ]
}"#,
        )
        .unwrap(),
    );
    let from_bpmn = normalize(
        &parse_and_compile(
            r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="minimal">
    <startEvent id="start"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#,
        )
        .unwrap(),
    );

    assert_eq!(from_builder, from_dsl);
    assert_eq!(from_builder, from_bpmn);
}

#[test]
fn builder_boundary_events_map_is_empty() {
    // The builder covers the shared node subset, which has no boundary
    // events; the compiled definition must reflect that with an empty map
    // (same as the DSL and a boundary-free BPMN document).
    let def = build_with_builder();
    assert!(def.boundary_events.is_empty());
}
