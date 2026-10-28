//! Happy-path compilation tests: every v1 node type, field defaults, edge
//! condition lowering, and edge-id auto-generation.

use bpm_engine_core::{EdgeCondition, FormFieldType, NodeType};
use bpm_engine_dsl::compile;

fn doc(nodes: &str, edges: &str) -> String {
    format!(
        r#"{{
  "schema_version": 1,
  "process": {{ "id": "happy", "name": "Happy" }},
  "nodes": {nodes},
  "edges": {edges}
}}"#
    )
}

#[test]
fn compile_minimal_start_end() {
    let json = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[ { "source": "start", "target": "end" } ]"#,
    );
    let def = compile(&json).unwrap();
    assert_eq!(def.id, "happy");
    assert_eq!(def.start, "start");
    assert_eq!(def.nodes.len(), 2);
    assert!(matches!(def.nodes["start"].node_type, NodeType::Start));
    assert!(matches!(def.nodes["end"].node_type, NodeType::End));
    assert_eq!(def.nodes["start"].outgoing_edges.len(), 1);
    assert_eq!(def.nodes["start"].outgoing_edges[0].target, "end");
    assert!(def.nodes["start"].outgoing_edges[0].condition.is_none());
}

#[test]
fn compile_service_task_with_defaults_matching_bpmn_parser() {
    // Omitted retries/timeout_secs fall back to the BPMN parser defaults
    // (retries = 3, timeout_secs = 60).
    let json = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "t", "type": "serviceTask", "task_type": "payment" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "t" },
    { "source": "t", "target": "end" }
  ]"#,
    );
    let def = compile(&json).unwrap();
    match &def.nodes["t"].node_type {
        NodeType::ExternalTask {
            task_type,
            retries,
            timeout_secs,
        } => {
            assert_eq!(task_type, "payment");
            assert_eq!(*retries, 3, "retries default must match BPMN parser");
            assert_eq!(*timeout_secs, 60, "timeout default must match BPMN parser");
        }
        other => panic!("expected ExternalTask, got {other:?}"),
    }
}

#[test]
fn compile_service_task_explicit_values_pass_through() {
    let json = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "t", "type": "serviceTask", "task_type": "work", "retries": 5, "timeout_secs": 120 },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "t" },
    { "source": "t", "target": "end" }
  ]"#,
    );
    let def = compile(&json).unwrap();
    match &def.nodes["t"].node_type {
        NodeType::ExternalTask {
            task_type,
            retries,
            timeout_secs,
        } => {
            assert_eq!(task_type, "work");
            assert_eq!(*retries, 5);
            assert_eq!(*timeout_secs, 120);
        }
        other => panic!("expected ExternalTask, got {other:?}"),
    }
}

#[test]
fn compile_user_task_with_form_defaults_matching_bpmn_parser() {
    // Label defaults to the field id; type defaults to string; required
    // defaults to false (BPMN parser behavior).
    let json = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "u", "type": "userTask", "form_key": "approval", "form_fields": [
      { "id": "amount", "type": "number", "required": true, "default_value": "0" },
      { "id": "reason", "label": "Reason" },
      { "id": "priority", "type": "enum", "options": ["high", "low"] }
    ] },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "u" },
    { "source": "u", "target": "end" }
  ]"#,
    );
    let def = compile(&json).unwrap();
    match &def.nodes["u"].node_type {
        NodeType::UserTask {
            form_key,
            form_fields,
        } => {
            assert_eq!(form_key.as_deref(), Some("approval"));
            let fields = form_fields.as_ref().unwrap();
            assert_eq!(fields.len(), 3);

            assert_eq!(fields[0].id, "amount");
            assert!(matches!(fields[0].field_type, FormFieldType::Number));
            assert!(fields[0].required);
            assert_eq!(fields[0].default_value.as_deref(), Some("0"));
            assert_eq!(fields[0].label, "amount", "label defaults to id");

            assert_eq!(fields[1].id, "reason");
            assert_eq!(fields[1].label, "Reason");
            assert!(!fields[1].required);
            assert!(matches!(fields[1].field_type, FormFieldType::String));

            assert!(matches!(fields[2].field_type, FormFieldType::Enum));
            assert_eq!(fields[2].options, Some(vec!["high".into(), "low".into()]));
        }
        other => panic!("expected UserTask, got {other:?}"),
    }
}

#[test]
fn compile_gateways_and_conditions() {
    let json = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "fork", "type": "parallelGateway" },
    { "id": "a", "type": "serviceTask", "task_type": "a" },
    { "id": "b", "type": "serviceTask", "task_type": "b" },
    { "id": "join", "type": "parallelGateway" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end1", "type": "end" },
    { "id": "end2", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "fork" },
    { "source": "fork", "target": "a" },
    { "source": "fork", "target": "b" },
    { "source": "a", "target": "join" },
    { "source": "b", "target": "join" },
    { "source": "join", "target": "xor" },
    { "source": "xor", "target": "end1", "condition": { "kind": "variableEq", "variable": "approved", "value": "true" } },
    { "source": "xor", "target": "end2", "condition": { "kind": "expression", "expression": "amount > 100" } }
  ]"#,
    );
    let def = compile(&json).unwrap();
    assert!(matches!(
        def.nodes["fork"].node_type,
        NodeType::ParallelFork
    ));
    match def.nodes["join"].node_type {
        NodeType::ParallelJoin { expected } => assert_eq!(expected, 2),
        ref other => panic!("expected ParallelJoin, got {other:?}"),
    }
    assert!(matches!(
        def.nodes["xor"].node_type,
        NodeType::ExclusiveGateway
    ));

    let edges = &def.nodes["xor"].outgoing_edges;
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0].target, "end1");
    match edges[0].condition.as_ref().unwrap() {
        EdgeCondition::Expression(expr) => {
            assert_eq!(expr, "approved == \"true\"");
        }
        other => panic!("expected Expression, got {other:?}"),
    }
    assert_eq!(edges[1].target, "end2");
    match edges[1].condition.as_ref().unwrap() {
        EdgeCondition::Expression(expr) => assert_eq!(expr, "amount > 100"),
        other => panic!("expected Expression, got {other:?}"),
    }
}

#[test]
fn compile_default_edge() {
    let json = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end1", "type": "end" },
    { "id": "end2", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "xor" },
    { "source": "xor", "target": "end1", "condition": { "kind": "expression", "expression": "amount > 100" } },
    { "source": "xor", "target": "end2", "default": true }
  ]"#,
    );
    let def = compile(&json).unwrap();
    let edges = &def.nodes["xor"].outgoing_edges;
    assert_eq!(edges.len(), 2);
    // The conditional edge keeps its expression.
    assert!(matches!(
        edges[0].condition.as_ref().unwrap(),
        EdgeCondition::Expression(_)
    ));
    // A default edge with no condition lowers to the same shape the BPMN
    // parser produces for a gateway default flow: no condition at all.
    assert_eq!(edges[1].target, "end2");
    assert!(edges[1].condition.is_none());
}

#[test]
fn compile_timer_intermediate_catch() {
    let json = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "w", "type": "timerIntermediateCatch", "timer": { "duration": "PT1H" } },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "w" },
    { "source": "w", "target": "end" }
  ]"#,
    );
    let def = compile(&json).unwrap();
    match &def.nodes["w"].node_type {
        NodeType::TimerIntermediateCatch { timer_definition } => {
            assert_eq!(timer_definition, "PT1H");
        }
        other => panic!("expected TimerIntermediateCatch, got {other:?}"),
    }
}

#[test]
fn compile_edge_id_optional_and_auto_generated() {
    // Edge ids are optional; the compiled graph is identical either way.
    let without_id = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[ { "source": "start", "target": "end" } ]"#,
    );
    let with_id = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "end", "type": "end" }
  ]"#,
        r#"[ { "id": "f1", "source": "start", "target": "end" } ]"#,
    );
    let a = compile(&without_id).unwrap();
    let b = compile(&with_id).unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(a.start, b.start);
    assert_eq!(a.nodes.len(), b.nodes.len());
}

#[test]
fn compile_expression_wrapper_is_stripped_like_bpmn() {
    // ${...}-wrapped expressions behave exactly like BPMN conditionExpression
    // text: the shared compiler strips the wrapper.
    let wrapped = doc(
        r#"[
    { "id": "start", "type": "start" },
    { "id": "xor", "type": "exclusiveGateway" },
    { "id": "end1", "type": "end" },
    { "id": "end2", "type": "end" }
  ]"#,
        r#"[
    { "source": "start", "target": "xor" },
    { "source": "xor", "target": "end1", "condition": { "kind": "expression", "expression": "${amount > 100}" } },
    { "source": "xor", "target": "end2", "default": true }
  ]"#,
    );
    let def = compile(&wrapped).unwrap();
    let edges = &def.nodes["xor"].outgoing_edges;
    match edges[0].condition.as_ref().unwrap() {
        EdgeCondition::Expression(expr) => assert_eq!(expr, "amount > 100"),
        other => panic!("expected Expression, got {other:?}"),
    }
}
