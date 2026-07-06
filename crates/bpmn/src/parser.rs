//! BPMN 2.0 XML parser → BpmnProcess (AST).

use crate::errors::ParseError;
use crate::model::{
    BoundaryEventType, BpmnFlowNode, BpmnProcess, BpmnSequenceFlow, FlowAttach, TimerType,
};
use bpm_engine_core::{FormField, FormFieldType};
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

    // Collect <bpmn:message> and <bpmn:signal> definitions for name resolution
    let mut message_defs: HashMap<String, String> = HashMap::new(); // id -> name
    let mut signal_defs: HashMap<String, String> = HashMap::new(); // id -> name
    for child in root.children() {
        if !child.is_element() {
            continue;
        }
        match child.tag_name().name() {
            "message" => {
                if let Some(id) = child.attribute("id") {
                    let name = child.attribute("name").unwrap_or(id).to_string();
                    message_defs.insert(id.to_string(), name);
                }
            }
            "signal" => {
                if let Some(id) = child.attribute("id") {
                    let name = child.attribute("name").unwrap_or(id).to_string();
                    signal_defs.insert(id.to_string(), name);
                }
            }
            _ => {}
        }
    }

    let process = doc
        .descendants()
        .find(|n| n.tag_name().name() == "process")
        .ok_or(ParseError::NoProcess)?;

    let process_id = process.attribute("id").unwrap_or("process").to_string();
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
            "subProcess" => {
                let node = parse_sub_process(&child, &message_defs, &signal_defs)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "intermediateCatchEvent" => {
                if let Some(node) =
                    parse_intermediate_catch_event(&child, &message_defs, &signal_defs)?
                {
                    flow_nodes.insert(node.id().to_string(), node);
                }
            }
            "intermediateThrowEvent" => {
                if let Some(node) =
                    parse_intermediate_throw_event(&child, &message_defs, &signal_defs)?
                {
                    flow_nodes.insert(node.id().to_string(), node);
                }
            }
            "callActivity" => {
                let node = parse_call_activity(&child)?;
                flow_nodes.insert(node.id().to_string(), node);
            }
            "boundaryEvent" => {
                let node = parse_boundary_event(&child)?;
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
/// Public so the compiler can re-wire after flattening subProcesses.
/// This function is idempotent: it clears existing lists before populating.
pub fn wire_flows(model: &mut BpmnProcess) {
    // Clear existing incoming/outgoing lists to make this idempotent
    for node in model.flow_nodes.values_mut() {
        node.clear_flows();
    }
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

    // Check for terminateEventDefinition
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        if child.tag_name().name() == "terminateEventDefinition" {
            return Ok(BpmnFlowNode::TerminateEndEvent {
                id,
                name,
                incoming: vec![],
                outgoing: vec![],
            });
        }
    }

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

    for ext in node
        .children()
        .filter(|n| n.tag_name().name() == "extensionElements")
    {
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
    for a in node.attributes() {
        if a.namespace() == Some(CAMUNDA_NS) && a.name() == "formKey" {
            form_key = Some(a.value().to_string());
        }
    }

    // Parse camunda:formData extension elements
    let mut form_fields = None::<Vec<FormField>>;
    for ext in node
        .children()
        .filter(|n| n.tag_name().name() == "extensionElements")
    {
        for e in ext.children() {
            if e.tag_name().name() == "formData"
                || (e.tag_name().namespace() == Some(CAMUNDA_NS)
                    && e.tag_name().name() == "formData")
            {
                let fields = parse_form_data(&e);
                if !fields.is_empty() {
                    form_fields = Some(fields);
                }
            }
        }
    }

    Ok(BpmnFlowNode::UserTask {
        id,
        name,
        form_key,
        form_fields,
        incoming: vec![],
        outgoing: vec![],
    })
}

/// Parse `<camunda:formData>` children into `FormField` list.
fn parse_form_data(node: &roxmltree::Node) -> Vec<FormField> {
    let mut fields = Vec::new();
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        if child.tag_name().name() != "formField" {
            continue;
        }
        let Some(field_id) = child.attribute("id") else {
            continue;
        };
        let label = child.attribute("label").unwrap_or(field_id).to_string();
        let type_str = child.attribute("type").unwrap_or("string");
        let field_type = match type_str {
            "long" | "number" | "int" | "integer" | "double" | "float" => FormFieldType::Number,
            "boolean" | "bool" => FormFieldType::Boolean,
            "enum" => FormFieldType::Enum,
            _ => FormFieldType::String,
        };
        let default_value = child.attribute("defaultValue").map(String::from);

        // Parse validation constraints for "required"
        let mut required = false;
        let mut options = Vec::new();
        for inner in child.children() {
            if !inner.is_element() {
                continue;
            }
            if inner.tag_name().name() == "validation" {
                for constraint in inner.children() {
                    if !constraint.is_element() {
                        continue;
                    }
                    if constraint.attribute("name") == Some("required") {
                        required = true;
                    }
                }
            }
            if inner.tag_name().name() == "value" {
                if let Some(val) = inner.attribute("id") {
                    options.push(val.to_string());
                }
            }
        }

        fields.push(FormField {
            id: field_id.to_string(),
            label,
            field_type,
            required,
            default_value,
            options: if options.is_empty() {
                None
            } else {
                Some(options)
            },
        });
    }
    fields
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

/// Parse a `<bpmn:boundaryEvent>` element.
///
/// Supports timer and error boundary events. The element must have an
/// `attachedToRef` attribute pointing to the host activity.
fn parse_boundary_event(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    let attached_to_ref = attr(node, "attachedToRef")?;
    let cancel_activity = node
        .attribute("cancelActivity")
        .map(|v| v == "true")
        .unwrap_or(true); // BPMN default: interrupting

    let mut event_type: Option<BoundaryEventType> = None;

    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        let tag = child.tag_name().name();
        match tag {
            "timerEventDefinition" => {
                let timer_type = parse_timer_definition(&child)?;
                event_type = Some(BoundaryEventType::Timer(timer_type));
            }
            "errorEventDefinition" => {
                let error_code = child.attribute("errorRef").map(String::from);
                event_type = Some(BoundaryEventType::Error { error_code });
            }
            _ => {}
        }
    }

    let event_type = event_type.ok_or_else(|| {
        ParseError::MissingAttribute(
            "boundaryEvent must have a timerEventDefinition or errorEventDefinition".to_string(),
        )
    })?;

    Ok(BpmnFlowNode::BoundaryEvent {
        id,
        name,
        attached_to_ref,
        event_type,
        is_interrupting: cancel_activity,
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

/// Parse a `<bpmn:subProcess>` element, recursively parsing internal flow nodes
/// and sequence flows.
fn parse_sub_process(
    node: &roxmltree::Node,
    message_defs: &HashMap<String, String>,
    signal_defs: &HashMap<String, String>,
) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");

    let mut inner_nodes: HashMap<String, BpmnFlowNode> = HashMap::new();
    let mut inner_flows: Vec<BpmnSequenceFlow> = Vec::new();

    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        let tag = child.tag_name().name();
        match tag {
            "startEvent" => {
                let n = parse_start_event(&child)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "endEvent" => {
                let n = parse_end_event(&child)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "serviceTask" => {
                let n = parse_service_task(&child)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "userTask" => {
                let n = parse_user_task(&child)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "exclusiveGateway" => {
                let n = parse_exclusive_gateway(&child)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "parallelGateway" => {
                let n = parse_parallel_gateway(&child)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "subProcess" => {
                let n = parse_sub_process(&child, message_defs, signal_defs)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "intermediateCatchEvent" => {
                if let Some(n) = parse_intermediate_catch_event(&child, message_defs, signal_defs)?
                {
                    inner_nodes.insert(n.id().to_string(), n);
                }
            }
            "intermediateThrowEvent" => {
                if let Some(n) = parse_intermediate_throw_event(&child, message_defs, signal_defs)?
                {
                    inner_nodes.insert(n.id().to_string(), n);
                }
            }
            "callActivity" => {
                let n = parse_call_activity(&child)?;
                inner_nodes.insert(n.id().to_string(), n);
            }
            "sequenceFlow" => {
                let flow = parse_sequence_flow(&child)?;
                inner_flows.push(flow);
            }
            _ => {}
        }
    }

    // Handle default flows on exclusive gateways inside the subProcess
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        if child.tag_name().name() == "exclusiveGateway" {
            if let Some(default_flow_id) = child.attribute("default") {
                for flow in &mut inner_flows {
                    if flow.id == default_flow_id {
                        flow.is_default = true;
                        break;
                    }
                }
            }
        }
    }

    Ok(BpmnFlowNode::SubProcess {
        id,
        name,
        flow_nodes: inner_nodes,
        sequence_flows: inner_flows,
        incoming: vec![],
        outgoing: vec![],
    })
}

/// Parse a `<bpmn:intermediateCatchEvent>`.
///
/// Supports timer, message, and signal catch events.
/// Returns `None` if it has no recognized event definition.
fn parse_intermediate_catch_event(
    node: &roxmltree::Node,
    message_defs: &HashMap<String, String>,
    signal_defs: &HashMap<String, String>,
) -> Result<Option<BpmnFlowNode>, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");

    let mut timer_type: Option<TimerType> = None;
    let mut message_name: Option<String> = None;
    let mut signal_name: Option<String> = None;

    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        match child.tag_name().name() {
            "timerEventDefinition" => {
                timer_type = Some(parse_timer_definition(&child)?);
            }
            "messageEventDefinition" => {
                message_name = resolve_message_ref(&child, message_defs);
            }
            "signalEventDefinition" => {
                signal_name = resolve_signal_ref(&child, signal_defs);
            }
            _ => {}
        }
    }

    if let Some(msg) = message_name {
        return Ok(Some(BpmnFlowNode::MessageIntermediateCatchEvent {
            id,
            name,
            message_name: msg,
            incoming: vec![],
            outgoing: vec![],
        }));
    }

    if let Some(sig) = signal_name {
        return Ok(Some(BpmnFlowNode::SignalIntermediateCatchEvent {
            id,
            name,
            signal_name: sig,
            incoming: vec![],
            outgoing: vec![],
        }));
    }

    let Some(timer_type) = timer_type else {
        return Ok(None);
    };

    Ok(Some(BpmnFlowNode::TimerIntermediateCatchEvent {
        id,
        name,
        timer_type,
        incoming: vec![],
        outgoing: vec![],
    }))
}

/// Parse a `<bpmn:intermediateThrowEvent>`.
///
/// Supports message and signal throw events.
/// Returns `None` if it has no recognized event definition.
fn parse_intermediate_throw_event(
    node: &roxmltree::Node,
    message_defs: &HashMap<String, String>,
    signal_defs: &HashMap<String, String>,
) -> Result<Option<BpmnFlowNode>, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");

    let mut message_name: Option<String> = None;
    let mut signal_name: Option<String> = None;

    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        match child.tag_name().name() {
            "messageEventDefinition" => {
                message_name = resolve_message_ref(&child, message_defs);
            }
            "signalEventDefinition" => {
                signal_name = resolve_signal_ref(&child, signal_defs);
            }
            _ => {}
        }
    }

    if let Some(msg) = message_name {
        return Ok(Some(BpmnFlowNode::MessageIntermediateThrowEvent {
            id,
            name,
            message_name: msg,
            incoming: vec![],
            outgoing: vec![],
        }));
    }

    if let Some(sig) = signal_name {
        return Ok(Some(BpmnFlowNode::SignalIntermediateThrowEvent {
            id,
            name,
            signal_name: sig,
            incoming: vec![],
            outgoing: vec![],
        }));
    }

    Ok(None)
}

/// Parse a `<bpmn:callActivity>` element.
fn parse_call_activity(node: &roxmltree::Node) -> Result<BpmnFlowNode, ParseError> {
    let id = attr(node, "id")?;
    let name = attr_opt(node, "name");
    let called_element = attr(node, "calledElement")?;
    Ok(BpmnFlowNode::CallActivity {
        id,
        name,
        called_element,
        incoming: vec![],
        outgoing: vec![],
    })
}

/// Resolve a `messageRef` attribute to a message name via the definitions-level lookup.
fn resolve_message_ref(
    node: &roxmltree::Node,
    message_defs: &HashMap<String, String>,
) -> Option<String> {
    node.attribute("messageRef")
        .and_then(|ref_id| message_defs.get(ref_id).cloned())
}

/// Resolve a `signalRef` attribute to a signal name via the definitions-level lookup.
fn resolve_signal_ref(
    node: &roxmltree::Node,
    signal_defs: &HashMap<String, String>,
) -> Option<String> {
    node.attribute("signalRef")
        .and_then(|ref_id| signal_defs.get(ref_id).cloned())
}

/// Parse the contents of `<bpmn:timerEventDefinition>`:
/// one of timeDuration, timeDate, or timeCycle.
fn parse_timer_definition(node: &roxmltree::Node) -> Result<TimerType, ParseError> {
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        let tag = child.tag_name().name();
        let text = child.text().unwrap_or("").trim().to_string();
        match tag {
            "timeDuration" => return Ok(TimerType::TimeDuration(text)),
            "timeDate" => return Ok(TimerType::TimeDate(text)),
            "timeCycle" => return Ok(TimerType::TimeCycle(text)),
            _ => {}
        }
    }
    Err(ParseError::MissingAttribute(
        "timerEventDefinition must contain timeDuration, timeDate, or timeCycle".to_string(),
    ))
}
