//! BPMN 2.0 conformance test suite (#27).
//!
//! Tests basic parsing and compilation of all supported BPMN elements,
//! boundary conditions, and error handling.

use bpm_engine_bpmn::{parse, parse_and_compile, ErrorCode};
use bpm_engine_core::NodeType;

// ===========================================================================
// Section 1: Supported BPMN element parsing
// ===========================================================================

#[test]
fn conformance_start_end_event() {
    let xml = include_str!("fixtures/minimal.bpmn");
    let def = parse_and_compile(xml).unwrap();
    assert_eq!(def.id, "minimal");
    assert_eq!(def.start, "start");
    assert!(def.nodes.contains_key("start"));
    assert!(def.nodes.contains_key("end"));
    match &def.nodes["start"].node_type {
        NodeType::Start => {}
        other => panic!("expected Start, got {:?}", other),
    }
    match &def.nodes["end"].node_type {
        NodeType::End => {}
        other => panic!("expected End, got {:?}", other),
    }
}

#[test]
fn conformance_service_task() {
    let xml = include_str!("fixtures/service_tasks.bpmn");
    let def = parse_and_compile(xml).unwrap();
    assert_eq!(def.id, "service-task-flow");
    assert_eq!(def.nodes.len(), 5); // start, task1, task2, task3, end

    for task_id in &["task1", "task2", "task3"] {
        let node = def.nodes.get(*task_id).unwrap();
        match &node.node_type {
            NodeType::ExternalTask { task_type, .. } => {
                assert_eq!(task_type, "default");
            }
            other => panic!("expected ExternalTask for {}, got {:?}", task_id, other),
        }
    }
}

#[test]
fn conformance_exclusive_gateway() {
    let xml = include_str!("fixtures/exclusive_gateway.bpmn");
    let def = parse_and_compile(xml).unwrap();
    assert_eq!(def.id, "xor-gateway");

    let decide = def.nodes.get("decide").unwrap();
    match &decide.node_type {
        NodeType::ExclusiveGateway => {}
        other => panic!("expected ExclusiveGateway, got {:?}", other),
    }
    // Should have 2 outgoing edges (approved + rejected default)
    assert_eq!(decide.outgoing_edges.len(), 2);
}

#[test]
fn conformance_parallel_gateway() {
    let xml = include_str!("fixtures/parallel_gateway.bpmn");
    let def = parse_and_compile(xml).unwrap();
    assert_eq!(def.id, "parallel-flow");

    let fork = def.nodes.get("fork").unwrap();
    match &fork.node_type {
        NodeType::ParallelFork => {}
        other => panic!("expected ParallelFork, got {:?}", other),
    }
    assert_eq!(fork.outgoing_edges.len(), 3);

    let join = def.nodes.get("join").unwrap();
    match &join.node_type {
        NodeType::ParallelJoin { expected } => {
            assert_eq!(*expected, 3);
        }
        other => panic!("expected ParallelJoin, got {:?}", other),
    }
}

#[test]
fn conformance_timer_intermediate_catch_duration() {
    let xml = include_str!("fixtures/timer_events.bpmn");
    let model = parse(xml).unwrap();

    match &model.flow_nodes["wait-duration"] {
        bpm_engine_bpmn::BpmnFlowNode::TimerIntermediateCatchEvent { timer_type, .. } => {
            match timer_type {
                bpm_engine_bpmn::model::TimerType::TimeDuration(d) => assert_eq!(d, "PT1H"),
                other => panic!("expected TimeDuration, got {:?}", other),
            }
        }
        other => panic!("expected TimerIntermediateCatchEvent, got {:?}", other),
    }
}

#[test]
fn conformance_timer_intermediate_catch_date() {
    let xml = include_str!("fixtures/timer_events.bpmn");
    let model = parse(xml).unwrap();

    match &model.flow_nodes["wait-date"] {
        bpm_engine_bpmn::BpmnFlowNode::TimerIntermediateCatchEvent { timer_type, .. } => {
            match timer_type {
                bpm_engine_bpmn::model::TimerType::TimeDate(d) => {
                    assert_eq!(d, "2025-06-01T00:00:00Z");
                }
                other => panic!("expected TimeDate, got {:?}", other),
            }
        }
        other => panic!("expected TimerIntermediateCatchEvent, got {:?}", other),
    }
}

#[test]
fn conformance_boundary_timer_event() {
    let xml = include_str!("fixtures/timer_events.bpmn");
    let def = parse_and_compile(xml).unwrap();

    let timeout = def.nodes.get("timeout").unwrap();
    match &timeout.node_type {
        NodeType::BoundaryTimer {
            duration,
            is_interrupting,
        } => {
            assert_eq!(duration, "PT30M");
            assert!(*is_interrupting);
        }
        other => panic!("expected BoundaryTimer, got {:?}", other),
    }

    // Boundary events map should link timeout to approval
    let boundary = def.boundary_events.get("approval").unwrap();
    assert_eq!(boundary.len(), 1);
    assert_eq!(boundary[0].node_id, "timeout");
    assert!(boundary[0].is_interrupting);
    assert_eq!(boundary[0].target_node_id, "end-timeout");
}

#[test]
fn conformance_subprocess_flattening() {
    let xml = include_str!("fixtures/subprocess.bpmn");
    let def = parse_and_compile(xml).unwrap();

    // Subprocess internal nodes should be flattened with namespace prefix
    assert!(def.nodes.contains_key("payment-sub:validate"));
    assert!(def.nodes.contains_key("payment-sub:charge"));

    // Subprocess start/end events should be removed
    assert!(!def.nodes.contains_key("payment-sub:sub_start"));
    assert!(!def.nodes.contains_key("payment-sub:sub_end"));

    // The subprocess node itself should not exist
    assert!(!def.nodes.contains_key("payment-sub"));

    // Edge wiring: start -> payment-sub:validate
    let start = def.nodes.get("start").unwrap();
    assert!(start
        .outgoing_edges
        .iter()
        .any(|e| e.target == "payment-sub:validate"));
}

#[test]
fn conformance_user_task_with_form() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
                 xmlns:camunda="http://camunda.org/schema/1.0/bpmn">
      <process id="form-flow">
        <startEvent id="start"/>
        <userTask id="task1" camunda:formKey="approval-form">
          <extensionElements>
            <camunda:formData>
              <camunda:formField id="amount" label="Amount" type="long" defaultValue="0">
                <camunda:validation>
                  <camunda:constraint name="required"/>
                </camunda:validation>
              </camunda:formField>
            </camunda:formData>
          </extensionElements>
        </userTask>
        <endEvent id="end"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="task1"/>
        <sequenceFlow id="f2" sourceRef="task1" targetRef="end"/>
      </process>
    </definitions>"#;

    let def = parse_and_compile(xml).unwrap();
    let task = def.nodes.get("task1").unwrap();
    match &task.node_type {
        NodeType::UserTask {
            form_key,
            form_fields,
        } => {
            assert_eq!(form_key.as_deref(), Some("approval-form"));
            let fields = form_fields.as_ref().unwrap();
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].id, "amount");
            assert!(fields[0].required);
        }
        other => panic!("expected UserTask, got {:?}", other),
    }
}

// ===========================================================================
// Section 2: Boundary conditions
// ===========================================================================

#[test]
fn conformance_single_node_process() {
    // A process with just start -> end (minimum valid)
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="single">
        <startEvent id="s"/>
        <endEvent id="e"/>
        <sequenceFlow id="f" sourceRef="s" targetRef="e"/>
      </process>
    </definitions>"#;
    let def = parse_and_compile(xml).unwrap();
    assert_eq!(def.nodes.len(), 2);
    assert_eq!(def.start, "s");
}

#[test]
fn conformance_many_sequential_tasks() {
    // Chain of 10 service tasks
    let mut xml = String::from(
        r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="chain">
        <startEvent id="start"/>
"#,
    );
    for i in 0..10 {
        xml.push_str(&format!(
            r#"        <serviceTask id="task{}" name="Task {}"/>
"#,
            i, i
        ));
    }
    xml.push_str(
        r#"        <endEvent id="end"/>
"#,
    );
    // Wire flows
    xml.push_str(
        r#"        <sequenceFlow id="fs" sourceRef="start" targetRef="task0"/>
"#,
    );
    for i in 0..9 {
        xml.push_str(&format!(
            r#"        <sequenceFlow id="f{}" sourceRef="task{}" targetRef="task{}"/>
"#,
            i,
            i,
            i + 1
        ));
    }
    xml.push_str(
        r#"        <sequenceFlow id="fe" sourceRef="task9" targetRef="end"/>
"#,
    );
    xml.push_str(
        r#"      </process>
    </definitions>"#,
    );

    let def = parse_and_compile(&xml).unwrap();
    assert_eq!(def.nodes.len(), 12); // start + 10 tasks + end
}

#[test]
fn conformance_multiple_end_events() {
    // Use parallel gateway to avoid exclusive gateway default-flow requirement
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="multi-end">
        <startEvent id="start"/>
        <parallelGateway id="gw"/>
        <endEvent id="end1"/>
        <endEvent id="end2"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
        <sequenceFlow id="f2" sourceRef="gw" targetRef="end1"/>
        <sequenceFlow id="f3" sourceRef="gw" targetRef="end2"/>
      </process>
    </definitions>"#;
    let def = parse_and_compile(xml).unwrap();
    assert!(def.nodes.contains_key("end1"));
    assert!(def.nodes.contains_key("end2"));
}

#[test]
fn conformance_gateway_with_default_flow() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="default-flow">
        <startEvent id="start"/>
        <exclusiveGateway id="gw" default="f-default"/>
        <serviceTask id="path-a" name="A"/>
        <endEvent id="end-a"/>
        <endEvent id="end-default"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
        <sequenceFlow id="f2" sourceRef="gw" targetRef="path-a">
          <conditionExpression>${x > 10}</conditionExpression>
        </sequenceFlow>
        <sequenceFlow id="f-default" sourceRef="gw" targetRef="end-default"/>
        <sequenceFlow id="f3" sourceRef="path-a" targetRef="end-a"/>
      </process>
    </definitions>"#;
    let def = parse_and_compile(xml).unwrap();
    let gw = def.nodes.get("gw").unwrap();
    // Should have 2 outgoing edges
    assert_eq!(gw.outgoing_edges.len(), 2);
}

// ===========================================================================
// Section 3: Error handling
// ===========================================================================

#[test]
fn conformance_error_invalid_xml() {
    let result = parse("this is not valid XML");
    assert!(result.is_err());
    match result.unwrap_err() {
        bpm_engine_bpmn::ParseError::InvalidXml(_) => {}
        other => panic!("expected InvalidXml, got {:?}", other),
    }
}

#[test]
fn conformance_error_no_process_element() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
    </definitions>"#;
    let result = parse(xml);
    assert!(result.is_err());
    match result.unwrap_err() {
        bpm_engine_bpmn::ParseError::NoProcess => {}
        other => panic!("expected NoProcess, got {:?}", other),
    }
}

#[test]
fn conformance_error_no_start_event() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <endEvent id="end"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs.iter().any(|e| e.code == ErrorCode::NoStartEvent));
}

#[test]
fn conformance_error_no_end_event() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <serviceTask id="t1"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="t1"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs.iter().any(|e| e.code == ErrorCode::NoEndEvent));
}

#[test]
fn conformance_error_orphan_node() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <endEvent id="end"/>
        <serviceTask id="orphan"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == ErrorCode::OrphanNode && e.node_id.as_deref() == Some("orphan")));
}

#[test]
fn conformance_error_dead_end_node() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <exclusiveGateway id="gw"/>
        <serviceTask id="dead"/>
        <endEvent id="end"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
        <sequenceFlow id="f2" sourceRef="gw" targetRef="dead"/>
        <sequenceFlow id="f3" sourceRef="gw" targetRef="end"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs.iter().any(|e| e.code == ErrorCode::DeadEnd));
}

#[test]
fn conformance_error_exclusive_gateway_no_default() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <exclusiveGateway id="gw"/>
        <endEvent id="end1"/>
        <endEvent id="end2"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="gw"/>
        <sequenceFlow id="f2" sourceRef="gw" targetRef="end1">
          <conditionExpression>true</conditionExpression>
        </sequenceFlow>
        <sequenceFlow id="f3" sourceRef="gw" targetRef="end2"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == ErrorCode::ExclusiveGatewayNoDefault));
}

#[test]
fn conformance_error_parallel_gateway_invalid_shape() {
    // Parallel gateway must be either fork (1 in, N out) or join (N in, 1 out)
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <parallelGateway id="pg"/>
        <endEvent id="end1"/>
        <endEvent id="end2"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="pg"/>
        <sequenceFlow id="f2" sourceRef="pg" targetRef="end1"/>
        <sequenceFlow id="f3" sourceRef="pg" targetRef="end2"/>
        <sequenceFlow id="f4" sourceRef="end1" targetRef="pg"/>
        <sequenceFlow id="f5" sourceRef="end2" targetRef="pg"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == ErrorCode::ParallelGatewayInvalidShape));
}

#[test]
fn conformance_error_subprocess_no_start() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <subProcess id="sub1">
          <endEvent id="sub_end"/>
          <sequenceFlow id="sf1" sourceRef="start" targetRef="sub_end"/>
        </subProcess>
        <endEvent id="end"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="sub1"/>
        <sequenceFlow id="f2" sourceRef="sub1" targetRef="end"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == ErrorCode::NoStartEvent && e.node_id.as_deref() == Some("sub1")));
}

#[test]
fn conformance_error_subprocess_no_end() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <subProcess id="sub1">
          <startEvent id="sub_start"/>
          <serviceTask id="sub_task"/>
          <sequenceFlow id="sf1" sourceRef="sub_start" targetRef="sub_task"/>
        </subProcess>
        <endEvent id="end"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="sub1"/>
        <sequenceFlow id="f2" sourceRef="sub1" targetRef="end"/>
      </process>
    </definitions>"#;
    let model = parse(xml).unwrap();
    let errs = bpm_engine_bpmn::compile(model).unwrap_err();
    assert!(errs
        .iter()
        .any(|e| e.code == ErrorCode::NoEndEvent && e.node_id.as_deref() == Some("sub1")));
}

// ===========================================================================
// Section 4: Parse error types
// ===========================================================================

#[test]
fn conformance_error_unknown_element_at_root() {
    let xml = r#"<not-definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p"/>
    </not-definitions>"#;
    let result = parse(xml);
    assert!(result.is_err());
    match result.unwrap_err() {
        bpm_engine_bpmn::ParseError::UnknownElement(name) => {
            assert_eq!(name, "not-definitions");
        }
        other => panic!("expected UnknownElement, got {:?}", other),
    }
}

// ===========================================================================
// Section 5: Compilation produces correct edge structure
// ===========================================================================

#[test]
fn conformance_edges_match_flow_definitions() {
    let xml = include_str!("fixtures/exclusive_gateway.bpmn");
    let def = parse_and_compile(xml).unwrap();

    // start -> decide
    let start = def.nodes.get("start").unwrap();
    assert_eq!(start.outgoing_edges.len(), 1);
    assert_eq!(start.outgoing_edges[0].target, "decide");

    // decide -> approved, rejected
    let decide = def.nodes.get("decide").unwrap();
    let targets: Vec<&str> = decide.outgoing_edges.iter().map(|e| e.target).collect();
    assert!(targets.contains(&"approved"));
    assert!(targets.contains(&"rejected"));
}

#[test]
fn conformance_parallel_fork_join_edges() {
    let xml = include_str!("fixtures/parallel_gateway.bpmn");
    let def = parse_and_compile(xml).unwrap();

    // fork -> branch-a, branch-b, branch-c
    let fork = def.nodes.get("fork").unwrap();
    assert_eq!(fork.outgoing_edges.len(), 3);

    // join -> end
    let join = def.nodes.get("join").unwrap();
    assert_eq!(join.outgoing_edges.len(), 1);
    assert_eq!(join.outgoing_edges[0].target, "end");

    // Each branch -> join
    for branch in &["branch-a", "branch-b", "branch-c"] {
        let node = def.nodes.get(*branch).unwrap();
        assert_eq!(node.outgoing_edges.len(), 1);
        assert_eq!(node.outgoing_edges[0].target, "join");
    }
}

// ===========================================================================
// Section 6: Timer compilation
// ===========================================================================

#[test]
fn conformance_timer_duration_compiles_to_node() {
    let xml = include_str!("fixtures/timer_events.bpmn");
    let def = parse_and_compile(xml).unwrap();

    let wait = def.nodes.get("wait-duration").unwrap();
    match &wait.node_type {
        NodeType::TimerIntermediateCatch { timer_definition } => {
            assert_eq!(timer_definition, "PT1H");
        }
        other => panic!("expected TimerIntermediateCatch, got {:?}", other),
    }
}

#[test]
fn conformance_boundary_error_event() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
      <process id="p">
        <startEvent id="start"/>
        <serviceTask id="task1"/>
        <boundaryEvent id="err1" attachedToRef="task1" cancelActivity="true">
          <errorEventDefinition errorRef="errCode1"/>
        </boundaryEvent>
        <endEvent id="end1"/>
        <endEvent id="end2"/>
        <sequenceFlow id="f1" sourceRef="start" targetRef="task1"/>
        <sequenceFlow id="f2" sourceRef="task1" targetRef="end1"/>
        <sequenceFlow id="f3" sourceRef="err1" targetRef="end2"/>
      </process>
    </definitions>"#;
    let def = parse_and_compile(xml).unwrap();
    let err_node = def.nodes.get("err1").unwrap();
    match &err_node.node_type {
        NodeType::BoundaryError {
            error_code,
            is_interrupting,
        } => {
            assert_eq!(error_code.as_deref(), Some("errCode1"));
            assert!(*is_interrupting);
        }
        other => panic!("expected BoundaryError, got {:?}", other),
    }
}

// ===========================================================================
// Section 7: Sequence flow condition expressions
// ===========================================================================

#[test]
fn conformance_condition_expressions_preserved() {
    let xml = include_str!("fixtures/exclusive_gateway.bpmn");
    let model = parse(xml).unwrap();

    // f2 should have a condition expression
    let f2 = model.sequence_flows.iter().find(|f| f.id == "f2").unwrap();
    assert!(f2.condition_expression.is_some());
    assert!(f2
        .condition_expression
        .as_ref()
        .unwrap()
        .contains("approved"));

    // f3 should be the default flow (no condition, no explicit default flag in this fixture)
    let f3 = model.sequence_flows.iter().find(|f| f.id == "f3").unwrap();
    // f3 has no condition expression — it's the implicit default
    assert!(f3.condition_expression.is_none());
}
