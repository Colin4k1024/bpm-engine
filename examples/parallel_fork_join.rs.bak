//! Parallel Fork/Join: Start → fork → (branch_a, branch_b) → join → End.
//!
//! Run: `cargo run --example parallel_fork_join`
//!
//! Demonstrates ParallelFork (one token in, N tokens out with same parallel_group_id)
//! and ParallelJoin (N tokens in, one out). Both branches run; when both have arrived at the join, one token continues to End.

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
use std::collections::HashMap;
use std::sync::Arc;

fn branch_a(_instance: &mut ProcessInstance) {
    println!("  branch_a");
}

fn branch_b(_instance: &mut ProcessInstance) {
    println!("  branch_b");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process = ProcessDefinition {
        id: "parallel_fork_join",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "fork",
                        condition: None,
                    }],
                },
            ),
            (
                "fork",
                Node {
                    id: "fork",
                    node_type: NodeType::ParallelFork,
                    outgoing_edges: vec![
                        OutgoingEdge {
                            target: "branch_a",
                            condition: None,
                        },
                        OutgoingEdge {
                            target: "branch_b",
                            condition: None,
                        },
                    ],
                },
            ),
            (
                "branch_a",
                Node {
                    id: "branch_a",
                    node_type: NodeType::ServiceTask(branch_a),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "join",
                        condition: None,
                    }],
                },
            ),
            (
                "branch_b",
                Node {
                    id: "branch_b",
                    node_type: NodeType::ServiceTask(branch_b),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "join",
                        condition: None,
                    }],
                },
            ),
            (
                "join",
                Node {
                    id: "join",
                    node_type: NodeType::ParallelJoin { expected: 2 },
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
        // Use in-memory join state so both branches converge at join and one token continues to end.
        parallel_join_repo: None,
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
    println!("OK: instance completed (Fork → branch_a, branch_b → Join → End)");
    Ok(())
}
