//! Integration tests for the typed process builder (`bpm_engine_bpmn::builder`).
//!
//! Covers: two-way structural parity with `parse_and_compile` on the same
//! flow (the three-way parity including the JSON DSL lives in the workspace
//! root `tests/builder_parity.rs`, where `bpm-engine-dsl` is available),
//! node-type lowering with BPMN-identical defaults, condition lowering, and
//! the error path — invalid builds surface the shared compiler's
//! `CompileError`s verbatim with their stable `ErrorCode`s.

use bpm_engine_bpmn::builder::{FormFieldSpec, ProcessBuilder};
use bpm_engine_bpmn::{parse_and_compile, CompileError, CompileErrors, ErrorCode};
use bpm_engine_core::NodeType;

/// A rich flow covering every builder node type, both condition kinds, the
/// default flag, form-field defaults and fork/join shapes.
const RICH_BPMN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
             xmlns:camunda="http://camunda.org/schema/1.0/bpmn">
  <process id="rich-flow" name="Rich Flow">
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

/// The builder equivalent of [`RICH_BPMN`]: same ids, same edge order, same
/// options; external-task timeout relies on the default (60), matching the
/// parser's hardcoded value.
fn build_rich_flow() -> bpm_engine_core::ProcessDefinition {
    ProcessBuilder::new("rich-flow")
        .process_name("Rich Flow")
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
        .edge("xor", "end-no")
        .is_default()
        .build()
        .expect("builder flow must compile")
}

/// Two-way parity: the builder and the XML parser compile the same flow to
/// the same normalized definition (node type debug form + outgoing edges in
/// wire order), the same comparison the DSL parity test uses.
#[test]
fn builder_and_bpmn_compile_to_identical_definitions() {
    let from_builder = build_rich_flow();
    let from_bpmn = parse_and_compile(RICH_BPMN).expect("BPMN must compile");

    assert_eq!(from_builder.id, from_bpmn.id);
    assert_eq!(from_builder.start, from_bpmn.start);
    assert_eq!(
        from_builder.nodes.len(),
        from_bpmn.nodes.len(),
        "node count must match"
    );
    for (id, node) in &from_builder.nodes {
        let other = from_bpmn
            .nodes
            .get(id)
            .unwrap_or_else(|| panic!("node {id} missing from BPMN build"));
        assert_eq!(
            format!("{:?}", node.node_type),
            format!("{:?}", other.node_type),
            "node type mismatch at {id}"
        );
        assert_eq!(
            node.outgoing_edges.len(),
            other.outgoing_edges.len(),
            "edge count mismatch at {id}"
        );
        for (a, b) in node.outgoing_edges.iter().zip(other.outgoing_edges.iter()) {
            assert_eq!(a.target, b.target, "edge target mismatch at {id}");
            assert_eq!(
                format!("{:?}", a.condition),
                format!("{:?}", b.condition),
                "edge condition mismatch at {id}"
            );
        }
    }
}

#[test]
fn builder_minimal_process_compiles() {
    let def = ProcessBuilder::new("minimal")
        .start_node("start")
        .end_node("end")
        .edge("start", "end")
        .build()
        .unwrap();

    assert_eq!(def.id, "minimal");
    assert_eq!(def.start, "start");
    assert_eq!(def.nodes.len(), 2);
    assert!(def.boundary_events.is_empty());
}

#[test]
fn builder_external_task_defaults_and_overrides() {
    // Default retries/timeout match the BPMN parser (3 / 60).
    let def = ProcessBuilder::new("p")
        .start_node("start")
        .external_task("defaulted", "orders")
        .external_task("tuned", "orders")
        .retries(7)
        .timeout_secs(300)
        .end_node("end")
        .edge("start", "defaulted")
        .edge("defaulted", "tuned")
        .edge("tuned", "end")
        .build()
        .unwrap();

    match &def.nodes["defaulted"].node_type {
        NodeType::ExternalTask {
            task_type,
            retries,
            timeout_secs,
        } => {
            assert_eq!(task_type, "orders");
            assert_eq!(*retries, 3);
            assert_eq!(*timeout_secs, 60);
        }
        _ => panic!("expected ExternalTask"),
    }
    match &def.nodes["tuned"].node_type {
        NodeType::ExternalTask {
            task_type,
            retries,
            timeout_secs,
        } => {
            assert_eq!(task_type, "orders");
            assert_eq!(*retries, 7);
            assert_eq!(*timeout_secs, 300);
        }
        _ => panic!("expected ExternalTask"),
    }
}

#[test]
fn builder_user_task_lowers_form_with_defaults() {
    let def = ProcessBuilder::new("p")
        .start_node("start")
        .user_task("approve")
        .form_key("approval-form")
        .form_field(FormFieldSpec::string("reason"))
        .form_field(FormFieldSpec::number("amount").required())
        .form_field(FormFieldSpec::enum_field("priority", &["high", "low"]))
        .end_node("end")
        .edge("start", "approve")
        .edge("approve", "end")
        .build()
        .unwrap();

    match &def.nodes["approve"].node_type {
        NodeType::UserTask {
            form_key,
            form_fields,
        } => {
            assert_eq!(form_key.as_deref(), Some("approval-form"));
            let fields = form_fields.as_ref().unwrap();
            assert_eq!(fields.len(), 3);
            // label defaults to id; type defaults to string; required
            // defaults to false (same as the BPMN parser / DSL).
            assert_eq!(fields[0].id, "reason");
            assert_eq!(fields[0].label, "reason");
            assert!(matches!(
                fields[0].field_type,
                bpm_engine_core::FormFieldType::String
            ));
            assert!(!fields[0].required);
            assert_eq!(fields[1].id, "amount");
            assert!(fields[1].required);
            assert_eq!(
                fields[2].options.as_deref(),
                Some(&["high".to_string(), "low".to_string()][..])
            );
        }
        _ => panic!("expected UserTask"),
    }
}

#[test]
fn builder_timer_catch_and_gateway_shapes() {
    let def = ProcessBuilder::new("p")
        .start_node("start")
        .timer_catch("wait", "PT1S")
        .parallel_gateway("fork")
        .end_node("end1")
        .end_node("end2")
        .edge("start", "wait")
        .edge("wait", "fork")
        .edge("fork", "end1")
        .edge("fork", "end2")
        .build()
        .unwrap();

    match &def.nodes["wait"].node_type {
        NodeType::TimerIntermediateCatch { timer_definition } => {
            assert_eq!(timer_definition, "PT1S");
        }
        _ => panic!("expected TimerIntermediateCatch"),
    }
    match &def.nodes["fork"].node_type {
        NodeType::ParallelFork => {}
        _ => panic!("expected ParallelFork"),
    }
}

#[test]
fn builder_conditions_lower_to_edge_conditions() {
    let def = ProcessBuilder::new("p")
        .start_node("start")
        .exclusive_gateway("xor")
        .end_node("end1")
        .end_node("end2")
        .end_node("end3")
        .edge("start", "xor")
        .edge("xor", "end1")
        .when_variable_eq("approved", "true")
        .edge("xor", "end2")
        .when_expression("amount > 100")
        .edge("xor", "end3")
        .is_default()
        .build()
        .unwrap();

    let xor = &def.nodes["xor"];
    assert_eq!(xor.outgoing_edges.len(), 3);
    match &xor.outgoing_edges[0].condition {
        Some(bpm_engine_core::EdgeCondition::Expression(e)) => {
            assert_eq!(e, "approved == \"true\"")
        }
        other => panic!("expected variableEq-lowered expression, got {other:?}"),
    }
    match &xor.outgoing_edges[1].condition {
        Some(bpm_engine_core::EdgeCondition::Expression(e)) => assert_eq!(e, "amount > 100"),
        other => panic!("expected expression condition, got {other:?}"),
    }
    match &xor.outgoing_edges[2].condition {
        // The shared compiler encodes a default flow as the absence of a
        // condition (same as for the XML and DSL entries).
        None => {}
        other => panic!("expected no condition on the default edge, got {other:?}"),
    }
}

/// Helper: extract the compiler error list out of the `CompileError` envelope
/// `build()` returns (same envelope as `parse_and_compile`).
fn error_codes(err: CompileError) -> Vec<ErrorCode> {
    match err {
        CompileError::Compile(CompileErrors(errors)) => errors.iter().map(|e| e.code).collect(),
        CompileError::Parse(e) => panic!("unexpected parse error: {e}"),
    }
}

#[test]
fn builder_error_no_end_node_returns_shared_code() {
    let err = ProcessBuilder::new("p")
        .start_node("start")
        .external_task("work", "orders")
        .edge("start", "work")
        .build()
        .unwrap_err();
    assert!(error_codes(err).contains(&ErrorCode::NoEndEvent));
}

#[test]
fn builder_error_multiple_start_events_returns_shared_code() {
    let err = ProcessBuilder::new("p")
        .start_node("s1")
        .start_node("s2")
        .end_node("end")
        .edge("s1", "end")
        .build()
        .unwrap_err();
    assert!(error_codes(err).contains(&ErrorCode::MultipleStartEvents));
}

#[test]
fn builder_error_dangling_edge_returns_shared_code() {
    let err = ProcessBuilder::new("p")
        .start_node("start")
        .end_node("end")
        .edge("start", "ghost")
        .build()
        .unwrap_err();
    let codes = error_codes(err);
    assert!(codes.contains(&ErrorCode::SequenceFlowTargetNotFound));
    // The other side is validated too: the edge itself is dangling, so the
    // shared compiler also flags the unreachable end node.
    assert!(codes.contains(&ErrorCode::OrphanNode));
}

#[test]
fn builder_error_gateway_without_default_returns_shared_code() {
    let err = ProcessBuilder::new("p")
        .start_node("start")
        .exclusive_gateway("xor")
        .end_node("end1")
        .end_node("end2")
        .edge("start", "xor")
        .edge("xor", "end1")
        .when_expression("x == 1")
        .edge("xor", "end2")
        .build()
        .unwrap_err();
    assert!(error_codes(err).contains(&ErrorCode::ExclusiveGatewayNoDefault));
}

#[test]
fn builder_error_dead_end_returns_shared_code() {
    let err = ProcessBuilder::new("p")
        .start_node("start")
        .exclusive_gateway("xor")
        .end_node("end")
        .external_task("dead", "orders")
        .edge("start", "xor")
        .edge("xor", "end")
        .is_default()
        .edge("xor", "dead")
        .build()
        .unwrap_err();
    assert!(error_codes(err).contains(&ErrorCode::DeadEnd));
}
