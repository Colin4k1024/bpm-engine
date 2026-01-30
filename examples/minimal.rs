//! Minimal example: Start → End.
//!
//! Run: `cargo run --example minimal`
//!
//! Demonstrates:
//! - Defining a process with two nodes (start, end).
//! - Creating engine context with in-memory SQLite.
//! - Starting a process and running until completion.

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
use std::collections::HashMap;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process = ProcessDefinition {
        id: "minimal",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
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
    println!("OK: instance {} completed (Start → End)", instance_id);
    Ok(())
}
