//! Approval example: Start → validate (ServiceTask) → gateway (ExclusiveGateway) → approve (UserTask) or reject (End) → end.
//!
//! Run: `cargo run --example approval`
//!
//! Uses an in-memory DB for the example; the default binary (`cargo run`) uses `bpm.db` and runs recovery on boot.

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler, UserTaskCompletedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
use bpm_engine::service;
use std::collections::HashMap;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process = ProcessDefinition {
        id: "approval",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "validate",
                        condition: None,
                    }],
                },
            ),
            (
                "validate",
                Node {
                    id: "validate",
                    node_type: NodeType::ServiceTask(service::validate),
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
        Box::new(UserTaskCompletedHandler),
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
    println!("Process started, paused at UserTask 'approve'.");

    engine.run(
        EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
            task_id: String::new(),
            instance_id: instance_id.clone(),
            node_id: "approve".into(),
            variables: HashMap::new(),
        }),
        &mut ctx,
    );

    let inst = repo.load(&instance_id).expect("instance exists");
    assert!(inst.completed());
    println!("OK: instance {} completed (approve path).", instance_id);
    Ok(())
}
