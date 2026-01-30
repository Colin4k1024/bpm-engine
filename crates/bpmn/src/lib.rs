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
pub fn parse_and_compile(xml: &str) -> Result<bpm_core::ProcessDefinition, CompileError> {
    let model = parse(xml).map_err(CompileError::Parse)?;
    compile(model).map_err(|v| CompileError::Compile(CompileErrors(v)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            bpm_core::NodeType::ExternalTask { task_type, .. } => assert_eq!(task_type, "default"),
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
            bpm_core::NodeType::ExclusiveGateway => {}
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
            bpm_core::NodeType::ParallelFork => {}
            _ => panic!("expected ParallelFork"),
        }
        match &join1.node_type {
            bpm_core::NodeType::ParallelJoin { expected } => assert_eq!(*expected, 2),
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
}
