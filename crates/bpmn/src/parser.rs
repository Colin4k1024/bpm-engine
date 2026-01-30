//! BPMN 2.0 XML parser → BpmnProcess (AST).

use crate::errors::ParseError;
use crate::model::{BpmnFlowNode, BpmnProcess, BpmnSequenceFlow, FlowAttach};
use roxmltree::Document;
use std::collections::HashMap;

const CAMUNDA_NS: &str = "http://camunda.org/schema/1.0/bpmn";

/// Parse BPMN 2.0 XML into a single process (first process in definitions).
pub fn parse(xml: &str) -> Result<BpmnProcess, ParseError> {
    let doc = Document::parse(xml).map_err(ParseError::InvalidXml)?;
    let root = doc.root_element();
    let tag = root.tag_name();
    let local = tag.name();
    if local != "definitions" {
        return Err(ParseError::UnknownElement(local.to_string()));
    }

    let process = doc
        .descendants()
        .find(|n| n.tag_name().name() == "process")
        .ok_or(ParseError::NoProcess)?;

    let process_id = process
        .attribute("id")
        .unwrap_or("process")
        .to_string();
    let process_name = process.attribute("name").map(String::from);

    let mut flow_nodes: HashMap<String, BpmnFlowNode> = HashMap::new();
    let mut sequence_flows: Vec<BpmnSequenceFlow> = Vec::new();

    for child in process.children() {
        if !child.is_element() {
            continue;
        }
        let name = child.tag_name().name();
        match name {
            "startEvent" => {
                let node = parse_start_event(&child)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "endEvent" => {
                let node = parse_end_event(&child)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "serviceTask" => {
                let node = parse_service_task(&child)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "userTask" => {
                let node = parse_user_task(&child)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "exclusiveGateway" => {
                let node = parse_exclusive_gateway(&child)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "parallelGateway" => {
                let node = parse_parallel_gateway(&child)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "sequenceFlow" => {
                let flow = parse_sequence_flow(&child)?;
                sequence_flows.push(flow);
            }
            "documentation" | "extensionElements" => {}
            _ => {}
        }
    }

    for child in process.children() {
        if !child.is_element() {
            continue;
        }
        if child.tag_name().name() == "exclusiveGateway" {
            if let Some(default_flow_id) = child.attribute("default") {
                for flow in &mut sequence_flows {
                    if flow.id == default_flow_id {
                        flow.is_default = true;
                        break;
                    }
                }
            }
        }
    }

    let mut model = BpmnProcess {
        id: process_id,
        name: process_name,
        flow_nodes,
        sequence_flows,
    };
    wire_flows(&mut model);
    Ok(model)
}

/// Attach flow IDs to nodes (01.md: Parser last step).
fn wire_flows(model: &mut BpmnProcess) {
    for flow in &model.sequence_flows {
        if let Some(node) = model.flow_nodes.get_mut(&flow.source_ref) {
            node.add_outgoing(&flow.id);
        }
        if let Some(node) = model.flow_nodes.get_mut(&flow.target_ref) {
            node.add_incoming(&flow.id);
        }
    }
}

fn attr(node: &roxmltree::Node, name: &str) -> Result<String, ParseError> {
    node.attribute(name)
        .map(String::from)
        .ok_or_else(|| ParseError::MissingAttribute(name.to_string()))
}

fn attr_opt(node: &roxmltree::Node, name: &str) -> Option<String> {
    node.attribute(name).map(String::from)
}

fn parse_start_event(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    Ok(BpmnFlowNode::StartEvent {
        id,
        name,
        incoming: vec![],
        outgoing: vec![],
    })
}

fn parse_end_event(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    Ok(BpmnFlowNode::EndEvent {
        id,
        name,
        incoming: vec![],
        outgoing: vec![],
    })
}

fn parse_service_task(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    let mut task_type = "default".to_string();
    let mut retries = 3i32;
    let timeout_secs = 60u64;

    for ext in node.children().filter(|n| n.tag_name().name() == "extensionElements") {
        for e in ext.children() {
            if e.tag_name().name() == "taskDefinition" {
                if let Some(ty) = e.attribute("type") {
                    task_type = ty.to_string();
                }
            }
            if e.tag_name().name() == "topic" {
                if let Some(ty) = e.attribute("topic").or_else(|| e.text()) {
                    task_type = ty.trim().to_string();
                }
            }
        }
    }
    for attr in node.attributes() {
        if attr.namespace() == Some(CAMUNDA_NS) && attr.name() == "topic" {
            task_type = attr.value().to_string();
        }
        if attr.namespace() == Some(CAMUNDA_NS) && attr.name() == "retries" {
            retries = attr.value().parse().unwrap_or(3);
        }
    }

    Ok(BpmnFlowNode::ServiceTask {
        id,
        name,
        task_type,
        retries,
        timeout_secs,
        incoming: vec![],
        outgoing: vec![],
    })
}

fn parse_user_task(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    let mut form_key = None::<String>;
    for attr in node.attributes() {
        if attr.namespace() == Some(CAMUNDA_NS) && attr.name() == "formKey" {
            form_key = Some(attr.value().to_string());
        }
    }
    Ok(BpmnFlowNode::UserTask {
        id,
        name,
        form_key,
        incoming: vec![],
        outgoing: vec![],
    })
}

fn parse_exclusive_gateway(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    Ok(BpmnFlowNode::ExclusiveGateway {
        id,
        name,
        incoming: vec![],
        outgoing: vec![],
    })
}

fn parse_parallel_gateway(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    Ok(BpmnFlowNode::ParallelGateway {
        id,
        name,
        incoming: vec![],
        outgoing: vec![],
    })
}

fn parse_sequence_flow(node: &roxmltree::Node) -> Result<BpmnSequenceFlow, ParseError> {
    let id = attr(node, "id")?;
    let source_ref = attr(node, "sourceRef")?;
    let target_ref = attr(node, "targetRef")?;
    let mut condition_expression = None;
    let mut is_default = false;
    for child in node.children() {
        if child.tag_name().name() == "conditionExpression" {
            condition_expression = child.text().map(|s| s.trim().to_string());
        }
    }
    if let Some(name) = node.attribute("name") {
        if name.eq_ignore_ascii_case("default") {
            is_default = true;
        }
    }
    Ok(BpmnSequenceFlow {
        id,
        source_ref,
        target_ref,
        condition_expression,
        is_default,
    })
}
