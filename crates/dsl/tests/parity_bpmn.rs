//! Parity test: a DSL document and its hand-written BPMN equivalent compile
//! to structurally identical `ProcessDefinition`s (ADR-006 acceptance bar).
//!
//! The comparison normalizes the compiled definitions (the `&'static str`
//! ids are leaked per compilation, so equality goes through owned strings
//! and the `Debug` form of `NodeType`/`EdgeCondition`).

use std::collections::BTreeMap;

use bpm_engine_bpmn::parse_and_compile;
use bpm_engine_core::ProcessDefinition;
use bpm_engine_dsl::compile;

/// A rich document covering every v1 node type, both condition kinds, the
/// default flag, form-field defaults, and gateway fork/join shapes.
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
    { "source": "xor", "target": "end-no", "default": true }
  ]
}"#;

/// The BPMN equivalent, field by field: same ids, same order, camunda
/// extension attributes for task type / retries / form, timer duration,
/// gateway `default` attribute, and `${amount == "100"}` condition text
/// (what the DSL's variableEq lowers to).
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
    <exclusiveGateway id="xor" default="flow8"/>
    <endEvent id="end-yes"/>
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
    <sequenceFlow id="flow8" sourceRef="xor" targetRef="end-no"/>
  </process>
</definitions>"#;

/// One normalized outgoing edge: (target node id, condition debug form).
type NormEdge = (String, Option<String>);

/// One normalized node: (node type debug form, outgoing edges in wire order).
type NormNode = (String, Vec<NormEdge>);

/// Normalized, comparable form of a compiled definition: the node map is
/// keyed by node id, values carry the `NodeType` debug form and outgoing
/// edges. Node ids are compared through the node's own id.
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
fn dsl_and_bpmn_compile_to_identical_definitions() {
    let from_dsl = compile(DSL_DOC).expect("DSL document must compile");
    let from_bpmn = parse_and_compile(BPMN_EQUIVALENT).expect("BPMN document must compile");

    let a = normalize(&from_dsl);
    let b = normalize(&from_bpmn);

    assert_eq!(a.id, b.id);
    assert_eq!(a.start, b.start);
    assert_eq!(
        a.nodes, b.nodes,
        "DSL and equivalent BPMN must compile identically:\n  dsl:  {:#?}\n  bpmn: {:#?}",
        a.nodes, b.nodes
    );
}

#[test]
fn minimal_dsl_and_bpmn_compile_to_identical_definitions() {
    let from_dsl = compile(
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
    .unwrap();
    let from_bpmn = parse_and_compile(
        r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="minimal">
    <startEvent id="start"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#,
    )
    .unwrap();

    assert_eq!(normalize(&from_dsl), normalize(&from_bpmn));
}

#[test]
fn boundary_events_map_is_empty_for_dsl_documents() {
    // DSL v1 has no boundary events; the compiled definition must reflect
    // that with an empty map (same as the BPMN doc without boundary events).
    let from_dsl = compile(
        r#"{
  "schema_version": 1,
  "process": { "id": "p" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ],
  "edges": [ { "source": "start", "target": "end" } ]
}"#,
    )
    .unwrap();
    assert!(from_dsl.boundary_events.is_empty());
}
