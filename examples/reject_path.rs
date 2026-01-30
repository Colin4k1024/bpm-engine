//! Reject path: Start → set_invalid (ServiceTask) → gateway → reject (End).
//!
//! Run: `cargo run --example reject_path`
//!
//! Same topology as approval, but the ServiceTask sets `valid = "false"` so the
//! ExclusiveGateway takes the Default edge to `reject` and the process ends without a UserTask.

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
use std::collections::HashMap;
use std::sync::Arc;

fn set_invalid(instance: &mut ProcessInstance) {
    instance.variables.insert("valid".into(), "false".into());
    println!("  set valid = false → gateway will take Default → reject");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process = ProcessDefinition {
        id: "reject_path",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "set_invalid",
                        condition: None,
                    }],
                },
            ),
            (
                "set_invalid",
                Node {
                    id: "set_invalid",
                    node_type: NodeType::ServiceTask(set_invalid),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "gateway",
                        condition: None,
                    }],
                },
            ),
            (
                "gateway",
                Node {
                    id: "gateway",
                    node_type: NodeType::ExclusiveGateway,
                    outgoing_edges: vec![
                        OutgoingEdge {
                            target: "approve",
                            condition: Some(EdgeCondition::VariableEq {
                                key: "valid".into(),
                                value: "true".into(),
                            }),
                        },
                        OutgoingEdge {
                            target: "reject",
                            condition: Some(EdgeCondition::Default),
                        },
                    ],
                },
            ),
            (
                "approve",
                Node {
                    id: "approve",
                    node_type: NodeType::UserTask,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "end",
                        condition: None,
                    }],
                },
            ),
            (
                "reject",
                Node {
                    id: "reject",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
            (
                "end",
                Node {
                    id: "end",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
        ]),
    };

    let repo = Arc::new(InstanceRepo::new(":memory:")?);
    let def_store = ProcessDefStore::new();
    def_store.register(process.clone());

    let mut ctx = EngineContext {
        process_repo: Some(Box::new(Arc::clone(&repo))),
        token_repo: Some(Box::new(Arc::clone(&repo))),
        process_def_repo: Some(Box::new(def_store)),
        task_repo: None,
        parallel_join_repo: Some(Box::new(Arc::clone(&repo))),
        timer_repo: Some(Box::new(Arc::clone(&repo))),
        compensation_repo: Some(Box::new(Arc::clone(&repo))),
        outbox_repo: None,
        tenant_id: None,
        run_in_tx: Some(Box::new(|event, handlers, ctx, queue| {
            for handler in handlers {
                let new_events = handler.handle(event, ctx);
                queue.extend(new_events);
            }
        })),
    };

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(ProcessCompletedHandler),
    ]);

    let instance_id = uuid::Uuid::new_v4().to_string();
    engine.run(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: process.id.to_string(),
            instance_id: instance_id.clone(),
            initial_variables: None,
        }),
        &mut ctx,
    );

    let inst = repo.load(&instance_id).expect("instance exists");
    assert!(inst.completed());
    println!("OK: instance completed on reject path (no UserTask)");
    Ok(())
}
