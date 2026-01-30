//! EL expression gateway: Start → set_vars (ServiceTask) → gateway (Expression conditions) → end_*.
//!
//! Run: `cargo run --example el_gateway`
//!
//! Demonstrates gateway conditions using EL expressions: string equality, numeric comparison,
//! and Default. Variables: choice="a", amount=100. First matching edge: choice == "a" → end_a.

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
use std::collections::HashMap;
use std::sync::Arc;

fn set_vars(instance: &mut ProcessInstance) {
    instance.variables.insert("choice".into(), "a".into());
    instance.variables.insert("amount".into(), "100".into());
    println!("  set choice=a, amount=100");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process = ProcessDefinition {
        id: "el_gateway",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "set_vars",
                        condition: None,
                    }],
                },
            ),
            (
                "set_vars",
                Node {
                    id: "set_vars",
                    node_type: NodeType::ServiceTask(set_vars),
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
                            target: "end_a",
                            condition: Some(EdgeCondition::Expression(r#"choice == "a""#.into())),
                        },
                        OutgoingEdge {
                            target: "end_high",
                            condition: Some(EdgeCondition::Expression("amount > 50".into())),
                        },
                        OutgoingEdge {
                            target: "end_b",
                            condition: Some(EdgeCondition::Default),
                        },
                    ],
                },
            ),
            (
                "end_a",
                Node {
                    id: "end_a",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
            (
                "end_high",
                Node {
                    id: "end_high",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
            (
                "end_b",
                Node {
                    id: "end_b",
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
        }),
        &mut ctx,
    );

    let inst = repo.load(&instance_id).expect("instance exists");
    assert!(inst.completed());
    println!("OK: instance completed at end_a (first matching EL: choice == \"a\")");
    Ok(())
}
