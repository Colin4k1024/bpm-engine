//! BPMN 2.0 XML parser and compiler to engine ProcessDefinition.
//! Definition layer only; runtime is unchanged.

pub mod compiler;
pub mod errors;
pub mod model;
pub mod parser;

pub use compiler::compile;
pub use errors::{CompileError, CompileErrors, CompilerError, ErrorCode, ParseError};
pub use model::{BpmnFlowNode, BpmnProcess, BpmnSequenceFlow, FlowAttach};
pub use parser::parse;

/// Parse BPMN XML and compile to engine ProcessDefinition in one step.
/// Returns Parse error or list of CompilerErrors (03.md).
pub fn parse_and_compile(xml: &str) -> Result<bpm_engine_core::ProcessDefinition, CompileError> {
    let model = parse(xml).map_err(CompileError::Parse)?;
    compile(model).map_err(|v| CompileError::Compile(CompileErrors(v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BoundaryEventType, TimerType};

    const MINIMAL_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="minimal" name="Minimal">
    <startEvent id="start" name="Start"/>
    <endEvent id="end" name="End"/>
    <sequenceFlow id="flow1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_minimal_bpmn() {
        let model = parse(MINIMAL_BPMN).unwrap();
        assert_eq!(model.id, "minimal");
        assert_eq!(model.flow_nodes.len(), 2);
        assert_eq!(model.sequence_flows.len(), 1);
    }

    #[test]
    fn compile_minimal_bpmn() {
        let def = parse_and_compile(MINIMAL_BPMN).unwrap();
        assert_eq!(def.id, "minimal");
        assert_eq!(def.start, "start");
        assert_eq!(def.nodes.len(), 2);
        assert!(def.nodes.contains_key("start"));
        assert!(def.nodes.contains_key("end"));
    }

    const SERVICE_TASK_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="payment-flow" name="Payment">
    <startEvent id="start"/>
    <serviceTask id="payment" name="Payment Task"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="payment"/>
    <sequenceFlow id="f2" sourceRef="payment" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn compile_service_task_bpmn() {
        let def = parse_and_compile(SERVICE_TASK_BPMN).unwrap();
        assert_eq!(def.id, "payment-flow");
        assert_eq!(def.start, "start");
        assert_eq!(def.nodes.len(), 3);
        let payment = def.nodes.get("payment").unwrap();
        match &payment.node_type {
            bpm_engine_core::NodeType::ExternalTask { task_type, .. } => {
                assert_eq!(task_type, "default")
            }
            _ => panic!("expected ExternalTask"),
        }
    }

    const GATEWAY_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="gateway-process">
    <startEvent id="start"/>
    <exclusiveGateway id="xor1" default="flowDefault"/>
    <endEvent id="end1"/>
    <endEvent id="end2"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="xor1"/>
    <sequenceFlow id="flowDefault" sourceRef="xor1" targetRef="end1"/>
    <sequenceFlow id="f2" sourceRef="xor1" targetRef="end2"/>
  </process>
</definitions>"#;

    #[test]
    fn compile_exclusive_gateway_bpmn() {
        let def = parse_and_compile(GATEWAY_BPMN).unwrap();
        assert_eq!(def.id, "gateway-process");
        assert!(def.nodes.contains_key("xor1"));
        let xor1 = def.nodes.get("xor1").unwrap();
        match &xor1.node_type {
            bpm_engine_core::NodeType::ExclusiveGateway => {}
            _ => panic!("expected ExclusiveGateway"),
        }
        assert_eq!(xor1.outgoing_edges.len(), 2);
    }

    const PARALLEL_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="parallel-process">
    <startEvent id="start"/>
    <parallelGateway id="fork1"/>
    <endEvent id="end1"/>
    <endEvent id="end2"/>
    <parallelGateway id="join1"/>
    <endEvent id="end3"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="fork1"/>
    <sequenceFlow id="f2" sourceRef="fork1" targetRef="end1"/>
    <sequenceFlow id="f3" sourceRef="fork1" targetRef="end2"/>
    <sequenceFlow id="f4" sourceRef="end1" targetRef="join1"/>
    <sequenceFlow id="f5" sourceRef="end2" targetRef="join1"/>
    <sequenceFlow id="f6" sourceRef="join1" targetRef="end3"/>
  </process>
</definitions>"#;

    #[test]
    fn compile_parallel_gateway_bpmn() {
        let def = parse_and_compile(PARALLEL_BPMN).unwrap();
        assert!(def.nodes.contains_key("fork1"));
        assert!(def.nodes.contains_key("join1"));
        let fork1 = def.nodes.get("fork1").unwrap();
        let join1 = def.nodes.get("join1").unwrap();
        match &fork1.node_type {
            bpm_engine_core::NodeType::ParallelFork => {}
            _ => panic!("expected ParallelFork"),
        }
        match &join1.node_type {
            bpm_engine_core::NodeType::ParallelJoin { expected } => assert_eq!(*expected, 2),
            _ => panic!("expected ParallelJoin"),
        }
    }

    // --- Compiler error tests (03.md: each ErrorCode at least one test) ---

    #[test]
    fn error_no_start_event() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        let errs = compile(model).unwrap_err();
        assert!(errs.iter().any(|e| e.code == ErrorCode::NoStartEvent));
    }

    #[test]
    fn error_multiple_start_events() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="s1"/>
    <startEvent id="s2"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="s1" targetRef="end"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::MultipleStartEvents));
    }

    #[test]
    fn error_no_end_event() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <task id="t1"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="t1"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        let errs = compile(model).unwrap_err();
        assert!(errs.iter().any(|e| e.code == ErrorCode::NoEndEvent));
    }

    #[test]
    fn error_orphan_node_no_incoming() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <endEvent id="end"/>
    <serviceTask id="orphan"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::OrphanNode && e.node_id.as_deref() == Some("orphan")));
    }

    #[test]
    fn error_sequence_flow_target_not_found() {
        use crate::model::{BpmnFlowNode, BpmnProcess, BpmnSequenceFlow};
        use std::collections::HashMap;
        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            BpmnFlowNode::StartEvent {
                id: "start".to_string(),
                name: None,
                incoming: vec![],
                outgoing: vec!["f1".to_string()],
            },
        );
        nodes.insert(
            "end".to_string(),
            BpmnFlowNode::EndEvent {
                id: "end".to_string(),
                name: None,
                incoming: vec!["f1".to_string()],
                outgoing: vec![],
            },
        );
        let model = BpmnProcess {
            id: "p".to_string(),
            name: None,
            flow_nodes: nodes,
            sequence_flows: vec![BpmnSequenceFlow {
                id: "f1".to_string(),
                source_ref: "start".to_string(),
                target_ref: "ghost".to_string(),
                condition_expression: None,
                is_default: false,
            }],
        };
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::SequenceFlowTargetNotFound));
    }

    #[test]
    fn error_sequence_flow_source_not_found() {
        use crate::model::{BpmnFlowNode, BpmnProcess, BpmnSequenceFlow};
        use std::collections::HashMap;
        let mut nodes = HashMap::new();
        nodes.insert(
            "end".to_string(),
            BpmnFlowNode::EndEvent {
                id: "end".to_string(),
                name: None,
                incoming: vec!["f1".to_string()],
                outgoing: vec![],
            },
        );
        let model = BpmnProcess {
            id: "p".to_string(),
            name: None,
            flow_nodes: nodes,
            sequence_flows: vec![BpmnSequenceFlow {
                id: "f1".to_string(),
                source_ref: "ghost".to_string(),
                target_ref: "end".to_string(),
                condition_expression: None,
                is_default: false,
            }],
        };
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::SequenceFlowSourceNotFound));
    }

    #[test]
    fn error_dead_end() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <exclusiveGateway id="xor1"/>
    <endEvent id="end"/>
    <serviceTask id="dead"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="xor1"/>
    <sequenceFlow id="f2" sourceRef="xor1" targetRef="end"/>
    <sequenceFlow id="f3" sourceRef="xor1" targetRef="dead"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        let errs = compile(model).unwrap_err();
        assert!(errs.iter().any(|e| e.code == ErrorCode::DeadEnd));
    }

    #[test]
    fn error_exclusive_gateway_no_default() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <exclusiveGateway id="xor1"/>
    <endEvent id="end1"/>
    <endEvent id="end2"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="xor1"/>
    <sequenceFlow id="f2" sourceRef="xor1" targetRef="end1">
      <conditionExpression>true</conditionExpression>
    </sequenceFlow>
    <sequenceFlow id="f3" sourceRef="xor1" targetRef="end2"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::ExclusiveGatewayNoDefault));
    }

    #[test]
    fn error_parallel_gateway_invalid_shape() {
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
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::ParallelGatewayInvalidShape));
    }

    // --- SubProcess tests (#13) ---

    const SUBPROCESS_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="subprocess-test" name="SubProcess Test">
    <startEvent id="start"/>
    <subProcess id="sub1" name="My SubProcess">
      <startEvent id="sub_start"/>
      <serviceTask id="sub_task" name="Inner Task"/>
      <endEvent id="sub_end"/>
      <sequenceFlow id="sf1" sourceRef="sub_start" targetRef="sub_task"/>
      <sequenceFlow id="sf2" sourceRef="sub_task" targetRef="sub_end"/>
    </subProcess>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="sub1"/>
    <sequenceFlow id="f2" sourceRef="sub1" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_subprocess() {
        let model = parse(SUBPROCESS_BPMN).unwrap();
        assert!(model.flow_nodes.contains_key("sub1"));
        match &model.flow_nodes["sub1"] {
            BpmnFlowNode::SubProcess {
                flow_nodes, name, ..
            } => {
                assert_eq!(name.as_deref(), Some("My SubProcess"));
                assert!(flow_nodes.contains_key("sub_start"));
                assert!(flow_nodes.contains_key("sub_task"));
                assert!(flow_nodes.contains_key("sub_end"));
            }
            _ => panic!("expected SubProcess"),
        }
    }

    #[test]
    fn compile_subprocess_flattens_nodes() {
        let def = parse_and_compile(SUBPROCESS_BPMN).unwrap();
        // After flattening: start, end, sub1:sub_task (start/end events removed)
        assert!(def.nodes.contains_key("start"));
        assert!(def.nodes.contains_key("end"));
        assert!(def.nodes.contains_key("sub1:sub_task"));
        // The subprocess node itself should NOT exist
        assert!(!def.nodes.contains_key("sub1"));
        // The internal start/end events should NOT exist
        assert!(!def.nodes.contains_key("sub1:sub_start"));
        assert!(!def.nodes.contains_key("sub1:sub_end"));
    }

    #[test]
    fn compile_subprocess_wires_edges_correctly() {
        let def = parse_and_compile(SUBPROCESS_BPMN).unwrap();
        // Debug: print all nodes and edges
        for (id, node) in &def.nodes {
            for edge in &node.outgoing_edges {
                eprintln!("  {} -> {}", id, edge.target);
            }
        }
        // start should have an edge to sub1:sub_task
        let start = def.nodes.get("start").unwrap();
        assert!(
            start
                .outgoing_edges
                .iter()
                .any(|e| e.target == "sub1:sub_task"),
            "start should have edge to sub1:sub_task, got: {:?}",
            start.outgoing_edges
        );
        // sub1:sub_task should have an edge to end
        let sub_task = def.nodes.get("sub1:sub_task").unwrap();
        assert!(
            sub_task.outgoing_edges.iter().any(|e| e.target == "end"),
            "sub1:sub_task should have edge to end, got: {:?}",
            sub_task.outgoing_edges
        );
    }

    #[test]
    fn error_subprocess_no_start_event() {
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
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::NoStartEvent && e.node_id.as_deref() == Some("sub1")));
    }

    #[test]
    fn error_subprocess_no_end_event() {
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
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::NoEndEvent && e.node_id.as_deref() == Some("sub1")));
    }

    // --- Timer event definition tests (#16) ---

    const TIMER_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="timer-test" name="Timer Test">
    <startEvent id="start"/>
    <intermediateCatchEvent id="timer1" name="Wait 1 Hour">
      <timerEventDefinition>
        <timeDuration>PT1H</timeDuration>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="timer1"/>
    <sequenceFlow id="f2" sourceRef="timer1" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_timer_duration() {
        let model = parse(TIMER_BPMN).unwrap();
        assert!(model.flow_nodes.contains_key("timer1"));
        match &model.flow_nodes["timer1"] {
            BpmnFlowNode::TimerIntermediateCatchEvent { timer_type, .. } => match timer_type {
                TimerType::TimeDuration(d) => assert_eq!(d, "PT1H"),
                _ => panic!("expected TimeDuration"),
            },
            _ => panic!("expected TimerIntermediateCatchEvent"),
        }
    }

    #[test]
    fn compile_timer_creates_timer_node() {
        let def = parse_and_compile(TIMER_BPMN).unwrap();
        assert!(def.nodes.contains_key("timer1"));
        let timer = def.nodes.get("timer1").unwrap();
        match &timer.node_type {
            bpm_engine_core::NodeType::TimerIntermediateCatch { timer_definition } => {
                assert_eq!(timer_definition, "PT1H");
            }
            _ => panic!("expected TimerIntermediateCatch"),
        }
    }

    #[test]
    fn parse_timer_date() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <intermediateCatchEvent id="timer1">
      <timerEventDefinition>
        <timeDate>2025-01-01T00:00:00Z</timeDate>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="timer1"/>
    <sequenceFlow id="f2" sourceRef="timer1" targetRef="end"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        match &model.flow_nodes["timer1"] {
            BpmnFlowNode::TimerIntermediateCatchEvent { timer_type, .. } => match timer_type {
                TimerType::TimeDate(d) => assert_eq!(d, "2025-01-01T00:00:00Z"),
                _ => panic!("expected TimeDate"),
            },
            _ => panic!("expected TimerIntermediateCatchEvent"),
        }
    }

    #[test]
    fn parse_timer_cycle() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <intermediateCatchEvent id="timer1">
      <timerEventDefinition>
        <timeCycle>R3/PT1H</timeCycle>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="timer1"/>
    <sequenceFlow id="f2" sourceRef="timer1" targetRef="end"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        match &model.flow_nodes["timer1"] {
            BpmnFlowNode::TimerIntermediateCatchEvent { timer_type, .. } => match timer_type {
                TimerType::TimeCycle(c) => assert_eq!(c, "R3/PT1H"),
                _ => panic!("expected TimeCycle"),
            },
            _ => panic!("expected TimerIntermediateCatchEvent"),
        }
    }

    #[test]
    fn error_subprocess_multiple_starts() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <subProcess id="sub1">
      <startEvent id="sub_start1"/>
      <startEvent id="sub_start2"/>
      <endEvent id="sub_end"/>
      <sequenceFlow id="sf1" sourceRef="sub_start1" targetRef="sub_end"/>
      <sequenceFlow id="sf2" sourceRef="sub_start2" targetRef="sub_end"/>
    </subProcess>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="sub1"/>
    <sequenceFlow id="f2" sourceRef="sub1" targetRef="end"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        let errs = compile(model).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.code == ErrorCode::MultipleStartEvents
                && e.node_id.as_deref() == Some("sub1")));
    }

    // --- Boundary event tests ---

    #[test]
    fn parse_timer_boundary_event() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
            xmlns:camunda="http://camunda.org/schema/1.0/bpmn">
  <process id="p">
    <startEvent id="start"/>
    <userTask id="task1" camunda:formKey="approval"/>
    <boundaryEvent id="timer1" attachedToRef="task1" cancelActivity="true">
      <timerEventDefinition>
        <timeDuration>PT30S</timeDuration>
      </timerEventDefinition>
    </boundaryEvent>
    <endEvent id="end1"/>
    <endEvent id="end2"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="task1"/>
    <sequenceFlow id="f2" sourceRef="task1" targetRef="end1"/>
    <sequenceFlow id="f3" sourceRef="timer1" targetRef="end2"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        assert!(model.flow_nodes.contains_key("timer1"));
        match &model.flow_nodes["timer1"] {
            BpmnFlowNode::BoundaryEvent {
                attached_to_ref,
                is_interrupting,
                event_type,
                ..
            } => {
                assert_eq!(attached_to_ref, "task1");
                assert!(is_interrupting);
                match event_type {
                    BoundaryEventType::Timer(t) => match t {
                        TimerType::TimeDuration(d) => assert_eq!(d, "PT30S"),
                        _ => panic!("expected TimeDuration"),
                    },
                    _ => panic!("expected Timer"),
                }
            }
            _ => panic!("expected BoundaryEvent"),
        }
    }

    #[test]
    fn compile_timer_boundary_event() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <userTask id="task1"/>
    <boundaryEvent id="timer1" attachedToRef="task1" cancelActivity="true">
      <timerEventDefinition>
        <timeDuration>PT1H</timeDuration>
      </timerEventDefinition>
    </boundaryEvent>
    <endEvent id="end1"/>
    <endEvent id="end2"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="task1"/>
    <sequenceFlow id="f2" sourceRef="task1" targetRef="end1"/>
    <sequenceFlow id="f3" sourceRef="timer1" targetRef="end2"/>
  </process>
</definitions>"#;
        let def = parse_and_compile(xml).unwrap();
        // Boundary event should be compiled as BoundaryTimer node
        let timer_node = def.nodes.get("timer1").unwrap();
        match &timer_node.node_type {
            bpm_engine_core::NodeType::BoundaryTimer {
                duration,
                is_interrupting,
            } => {
                assert_eq!(duration, "PT1H");
                assert!(is_interrupting);
            }
            _ => panic!("expected BoundaryTimer"),
        }
        // Boundary events map should link timer1 to task1
        let boundary = def.boundary_events.get("task1").unwrap();
        assert_eq!(boundary.len(), 1);
        assert_eq!(boundary[0].node_id, "timer1");
        assert!(boundary[0].is_interrupting);
        assert_eq!(boundary[0].target_node_id, "end2");
    }

    #[test]
    fn compile_error_boundary_event() {
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
            bpm_engine_core::NodeType::BoundaryError {
                error_code,
                is_interrupting,
            } => {
                assert_eq!(error_code.as_deref(), Some("errCode1"));
                assert!(is_interrupting);
            }
            _ => panic!("expected BoundaryError"),
        }
    }

    // --- Form field tests ---

    #[test]
    fn parse_user_task_with_form_data() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
            xmlns:camunda="http://camunda.org/schema/1.0/bpmn">
  <process id="p">
    <startEvent id="start"/>
    <userTask id="task1" camunda:formKey="approval-form">
      <extensionElements>
        <camunda:formData>
          <camunda:formField id="amount" label="Amount" type="long" defaultValue="0">
            <camunda:validation>
              <camunda:constraint name="required"/>
            </camunda:validation>
          </camunda:formField>
          <camunda:formField id="reason" label="Reason" type="string"/>
          <camunda:formField id="priority" label="Priority" type="enum">
            <camunda:value id="high" name="High"/>
            <camunda:value id="low" name="Low"/>
          </camunda:formField>
        </camunda:formData>
      </extensionElements>
    </userTask>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="task1"/>
    <sequenceFlow id="f2" sourceRef="task1" targetRef="end"/>
  </process>
</definitions>"#;
        let model = parse(xml).unwrap();
        match &model.flow_nodes["task1"] {
            BpmnFlowNode::UserTask {
                form_key,
                form_fields,
                ..
            } => {
                assert_eq!(form_key.as_deref(), Some("approval-form"));
                let fields = form_fields.as_ref().unwrap();
                assert_eq!(fields.len(), 3);

                // amount field
                assert_eq!(fields[0].id, "amount");
                assert_eq!(fields[0].label, "Amount");
                assert!(fields[0].required);
                assert_eq!(fields[0].default_value.as_deref(), Some("0"));
                match &fields[0].field_type {
                    bpm_engine_core::FormFieldType::Number => {}
                    _ => panic!("expected Number type"),
                }

                // reason field
                assert_eq!(fields[1].id, "reason");
                assert!(!fields[1].required);

                // priority field (enum)
                assert_eq!(fields[2].id, "priority");
                match &fields[2].field_type {
                    bpm_engine_core::FormFieldType::Enum => {}
                    _ => panic!("expected Enum type"),
                }
                let opts = fields[2].options.as_ref().unwrap();
                assert_eq!(opts, &vec!["high".to_string(), "low".to_string()]);
            }
            _ => panic!("expected UserTask"),
        }
    }

    #[test]
    fn compile_user_task_with_form_fields() {
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL"
            xmlns:camunda="http://camunda.org/schema/1.0/bpmn">
  <process id="p">
    <startEvent id="start"/>
    <userTask id="task1" camunda:formKey="my-form">
      <extensionElements>
        <camunda:formData>
          <camunda:formField id="name" label="Name" type="string">
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
            bpm_engine_core::NodeType::UserTask {
                form_key,
                form_fields,
            } => {
                assert_eq!(form_key.as_deref(), Some("my-form"));
                let fields = form_fields.as_ref().unwrap();
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].id, "name");
                assert!(fields[0].required);
            }
            _ => panic!("expected UserTask"),
        }
    }

    // --- Process Definition Version Management Tests ---

    #[tokio::test]
    async fn version_management_register_creates_version() {
        use bpm_engine_adapter_memory::ProcessDefStore;
        use bpm_engine_storage::{DefinitionStatus, ProcessDefinitionStore};

        let store = ProcessDefStore::new();
        let def = parse_and_compile(MINIMAL_BPMN).unwrap();
        let id = def.id.to_string();
        store.register(def);

        let versions = store.list_versions(&id).await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].id, id);
        assert_eq!(versions[0].status, DefinitionStatus::Active);
    }

    #[tokio::test]
    async fn version_management_activate_deprecates_previous() {
        use bpm_engine_adapter_memory::ProcessDefStore;
        use bpm_engine_storage::{DefinitionStatus, ProcessDefinitionStore};

        let store = ProcessDefStore::new();
        // Register v1
        let xml_v1 = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="order-flow:1">
            <startEvent id="start"/>
            <endEvent id="end"/>
            <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
          </process>
        </definitions>"#;
        let def1 = parse_and_compile(xml_v1).unwrap();
        store.register(def1);

        // Register v2 (same key)
        let xml_v2 = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="order-flow:2">
            <startEvent id="start"/>
            <endEvent id="end"/>
            <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
          </process>
        </definitions>"#;
        let def2 = parse_and_compile(xml_v2).unwrap();
        store.register(def2);

        let versions = store.list_versions("order-flow").await.unwrap();
        assert_eq!(versions.len(), 2);

        // v2 should be active, v1 should be deprecated
        let v1 = versions.iter().find(|v| v.id == "order-flow:1").unwrap();
        let v2 = versions.iter().find(|v| v.id == "order-flow:2").unwrap();
        assert_eq!(v1.status, DefinitionStatus::Deprecated);
        assert_eq!(v2.status, DefinitionStatus::Active);
    }

    #[tokio::test]
    async fn version_management_get_active() {
        use bpm_engine_adapter_memory::ProcessDefStore;
        use bpm_engine_storage::ProcessDefinitionStore;

        let store = ProcessDefStore::new();
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="my-flow:1">
            <startEvent id="start"/>
            <endEvent id="end"/>
            <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
          </process>
        </definitions>"#;
        let def = parse_and_compile(xml).unwrap();
        store.register(def);

        let active = store.get_active("my-flow").await.unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().id, "my-flow:1");
    }

    #[tokio::test]
    async fn version_management_activate_specific_version() {
        use bpm_engine_adapter_memory::ProcessDefStore;
        use bpm_engine_storage::{DefinitionStatus, ProcessDefinitionStore};

        let store = ProcessDefStore::new();
        // Register v1
        let xml_v1 = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="flow:1">
            <startEvent id="start"/>
            <endEvent id="end"/>
            <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
          </process>
        </definitions>"#;
        store.register(parse_and_compile(xml_v1).unwrap());

        // Register v2
        let xml_v2 = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="flow:2">
            <startEvent id="start"/>
            <endEvent id="end"/>
            <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
          </process>
        </definitions>"#;
        store.register(parse_and_compile(xml_v2).unwrap());

        // Activate v1
        store.activate("flow:1").await.unwrap();

        let active = store.get_active("flow").await.unwrap().unwrap();
        assert_eq!(active.id, "flow:1");
        assert_eq!(active.status, DefinitionStatus::Active);

        // v2 should be deprecated
        let versions = store.list_versions("flow").await.unwrap();
        let v2 = versions.iter().find(|v| v.id == "flow:2").unwrap();
        assert_eq!(v2.status, DefinitionStatus::Deprecated);
    }

    #[tokio::test]
    async fn version_management_deprecate() {
        use bpm_engine_adapter_memory::ProcessDefStore;
        use bpm_engine_storage::{DefinitionStatus, ProcessDefinitionStore};

        let store = ProcessDefStore::new();
        let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
          <process id="dep-flow:1">
            <startEvent id="start"/>
            <endEvent id="end"/>
            <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
          </process>
        </definitions>"#;
        store.register(parse_and_compile(xml).unwrap());

        store.deprecate("dep-flow:1").await.unwrap();

        let versions = store.list_versions("dep-flow").await.unwrap();
        assert_eq!(versions[0].status, DefinitionStatus::Deprecated);

        // get_active should return None
        let active = store.get_active("dep-flow").await.unwrap();
        assert!(active.is_none());
    }

    #[tokio::test]
    async fn version_management_activate_nonexistent_errors() {
        use bpm_engine_adapter_memory::ProcessDefStore;
        use bpm_engine_storage::ProcessDefinitionStore;

        let store = ProcessDefStore::new();
        let result = store.activate("nonexistent:1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn version_management_deprecate_nonexistent_errors() {
        use bpm_engine_adapter_memory::ProcessDefStore;
        use bpm_engine_storage::ProcessDefinitionStore;

        let store = ProcessDefStore::new();
        let result = store.deprecate("nonexistent:1").await;
        assert!(result.is_err());
    }

    // --- CallActivity tests (#14) ---

    const CALL_ACTIVITY_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="parent-process" name="Parent">
    <startEvent id="start"/>
    <callActivity id="call1" name="Call Sub Process" calledElement="sub-process"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="call1"/>
    <sequenceFlow id="f2" sourceRef="call1" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_call_activity() {
        let model = parse(CALL_ACTIVITY_BPMN).unwrap();
        assert!(model.flow_nodes.contains_key("call1"));
        match &model.flow_nodes["call1"] {
            BpmnFlowNode::CallActivity {
                called_element,
                name,
                ..
            } => {
                assert_eq!(called_element, "sub-process");
                assert_eq!(name.as_deref(), Some("Call Sub Process"));
            }
            _ => panic!("expected CallActivity"),
        }
    }

    #[test]
    fn compile_call_activity_creates_node() {
        let def = parse_and_compile(CALL_ACTIVITY_BPMN).unwrap();
        assert!(def.nodes.contains_key("call1"));
        let call = def.nodes.get("call1").unwrap();
        match &call.node_type {
            bpm_engine_core::NodeType::CallActivity { called_process_key } => {
                assert_eq!(called_process_key, "sub-process");
            }
            _ => panic!("expected CallActivity"),
        }
        // Should have edge to end
        assert!(call.outgoing_edges.iter().any(|e| e.target == "end"));
    }

    // --- Message event tests (#15) ---

    const MESSAGE_CATCH_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <message id="msg1" name="OrderReceived"/>
  <process id="msg-test" name="Message Test">
    <startEvent id="start"/>
    <intermediateCatchEvent id="catch1" name="Wait for Order">
      <messageEventDefinition messageRef="msg1"/>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="catch1"/>
    <sequenceFlow id="f2" sourceRef="catch1" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_message_catch_event() {
        let model = parse(MESSAGE_CATCH_BPMN).unwrap();
        assert!(model.flow_nodes.contains_key("catch1"));
        match &model.flow_nodes["catch1"] {
            BpmnFlowNode::MessageIntermediateCatchEvent { message_name, .. } => {
                assert_eq!(message_name, "OrderReceived");
            }
            _ => panic!("expected MessageIntermediateCatchEvent"),
        }
    }

    #[test]
    fn compile_message_catch_creates_node() {
        let def = parse_and_compile(MESSAGE_CATCH_BPMN).unwrap();
        let node = def.nodes.get("catch1").unwrap();
        match &node.node_type {
            bpm_engine_core::NodeType::MessageIntermediateCatch { message_name } => {
                assert_eq!(message_name, "OrderReceived");
            }
            _ => panic!("expected MessageIntermediateCatch"),
        }
    }

    const MESSAGE_THROW_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <message id="msg1" name="OrderConfirmed"/>
  <process id="msg-throw-test">
    <startEvent id="start"/>
    <intermediateThrowEvent id="throw1" name="Send Confirmation">
      <messageEventDefinition messageRef="msg1"/>
    </intermediateThrowEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="throw1"/>
    <sequenceFlow id="f2" sourceRef="throw1" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_message_throw_event() {
        let model = parse(MESSAGE_THROW_BPMN).unwrap();
        match &model.flow_nodes["throw1"] {
            BpmnFlowNode::MessageIntermediateThrowEvent { message_name, .. } => {
                assert_eq!(message_name, "OrderConfirmed");
            }
            _ => panic!("expected MessageIntermediateThrowEvent"),
        }
    }

    #[test]
    fn compile_message_throw_creates_node() {
        let def = parse_and_compile(MESSAGE_THROW_BPMN).unwrap();
        let node = def.nodes.get("throw1").unwrap();
        match &node.node_type {
            bpm_engine_core::NodeType::MessageIntermediateThrow { message_name } => {
                assert_eq!(message_name, "OrderConfirmed");
            }
            _ => panic!("expected MessageIntermediateThrow"),
        }
    }

    // --- Signal event tests ---

    const SIGNAL_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <signal id="sig1" name="AlertSignal"/>
  <process id="signal-test">
    <startEvent id="start"/>
    <intermediateThrowEvent id="throw1" name="Fire Alert">
      <signalEventDefinition signalRef="sig1"/>
    </intermediateThrowEvent>
    <intermediateCatchEvent id="catch1" name="Wait for Alert">
      <signalEventDefinition signalRef="sig1"/>
    </intermediateCatchEvent>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="throw1"/>
    <sequenceFlow id="f2" sourceRef="throw1" targetRef="catch1"/>
    <sequenceFlow id="f3" sourceRef="catch1" targetRef="end"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_signal_events() {
        let model = parse(SIGNAL_BPMN).unwrap();
        match &model.flow_nodes["throw1"] {
            BpmnFlowNode::SignalIntermediateThrowEvent { signal_name, .. } => {
                assert_eq!(signal_name, "AlertSignal");
            }
            _ => panic!("expected SignalIntermediateThrowEvent"),
        }
        match &model.flow_nodes["catch1"] {
            BpmnFlowNode::SignalIntermediateCatchEvent { signal_name, .. } => {
                assert_eq!(signal_name, "AlertSignal");
            }
            _ => panic!("expected SignalIntermediateCatchEvent"),
        }
    }

    #[test]
    fn compile_signal_events() {
        let def = parse_and_compile(SIGNAL_BPMN).unwrap();
        match &def.nodes.get("throw1").unwrap().node_type {
            bpm_engine_core::NodeType::SignalIntermediateThrow { signal_name } => {
                assert_eq!(signal_name, "AlertSignal");
            }
            _ => panic!("expected SignalIntermediateThrow"),
        }
        match &def.nodes.get("catch1").unwrap().node_type {
            bpm_engine_core::NodeType::SignalIntermediateCatch { signal_name } => {
                assert_eq!(signal_name, "AlertSignal");
            }
            _ => panic!("expected SignalIntermediateCatch"),
        }
    }

    // --- Terminate end event tests ---

    const TERMINATE_BPMN: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="terminate-test">
    <startEvent id="start"/>
    <endEvent id="end1">
      <terminateEventDefinition/>
    </endEvent>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end1"/>
  </process>
</definitions>"#;

    #[test]
    fn parse_terminate_end_event() {
        let model = parse(TERMINATE_BPMN).unwrap();
        match &model.flow_nodes["end1"] {
            BpmnFlowNode::TerminateEndEvent { .. } => {}
            _ => panic!("expected TerminateEndEvent"),
        }
    }

    #[test]
    fn compile_terminate_end_event() {
        let def = parse_and_compile(TERMINATE_BPMN).unwrap();
        match &def.nodes.get("end1").unwrap().node_type {
            bpm_engine_core::NodeType::TerminateEnd => {}
            _ => panic!("expected TerminateEnd"),
        }
    }
}
