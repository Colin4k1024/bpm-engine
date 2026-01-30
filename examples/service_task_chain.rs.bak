//! ServiceTask chain: Start → step1 → step2 → step3 → End.
//!
//! Run: `cargo run --example service_task_chain`
//!
//! Demonstrates a linear sequence of ServiceTasks; each step runs in order and the process completes in one run.

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
use std::collections::HashMap;
use std::sync::Arc;

fn step1(instance: &mut ProcessInstance) {
    instance.variables.insert("step".into(), "1".into());
    println!("  step1");
}

fn step2(instance: &mut ProcessInstance) {
    instance.variables.insert("step".into(), "2".into());
    println!("  step2");
}

fn step3(instance: &mut ProcessInstance) {
    instance.variables.insert("step".into(), "3".into());
    println!("  step3");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process = ProcessDefinition {
        id: "service_task_chain",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "step1",
                        condition: None,
                    }],
                },
            ),
            (
                "step1",
                Node {
                    id: "step1",
                    node_type: NodeType::ServiceTask(step1),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "step2",
                        condition: None,
                    }],
                },
            ),
            (
                "step2",
                Node {
                    id: "step2",
                    node_type: NodeType::ServiceTask(step2),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "step3",
                        condition: None,
                    }],
                },
            ),
            (
                "step3",
                Node {
                    id: "step3",
                    node_type: NodeType::ServiceTask(step3),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "end",
                        condition: None,
                    }],
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
    assert_eq!(inst.variables.get("step").map(String::as_str), Some("3"));
    println!("OK: instance completed after step1 → step2 → step3");
    Ok(())
}
