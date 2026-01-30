//! Default binary: runs the approval demo (Start → validate → gateway → approve/reject → end).
//! See README and `cargo run --example minimal` for other examples.

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler, UserTaskCompletedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore};
use bpm_engine::recovery;
use bpm_engine::service;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

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

    let repo = Arc::new(InstanceRepo::new("bpm.db")?);
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

    let mut recover_queue = VecDeque::new();
    if let Some(ref pr) = ctx.process_repo {
        recovery::recover(&**pr, ctx.token_repo.as_deref(), ctx.tenant_id.as_deref(), &mut recover_queue);
    }
    for ev in recover_queue {
        engine.run(ev, &mut ctx);
    }

    let instance_id = uuid::Uuid::new_v4().to_string();
    engine.run(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: process.id.to_string(),
            instance_id: instance_id.clone(),
            initial_variables: None,
        }),
        &mut ctx,
    );

    println!("--- 人工审批完成 ---");

    engine.run(
        EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
            task_id: String::new(),
            instance_id: instance_id.clone(),
            node_id: "approve".into(),
            variables: HashMap::new(),
        }),
        &mut ctx,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use bpm_engine::engine::{
        payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
        TokenArrivedHandler,
    };
    use bpm_engine::model::*;
    use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn engine_start_end_completes_instance() {
        let minimal = ProcessDefinition {
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
        let repo = Arc::new(InstanceRepo::new(":memory:").unwrap());
        let def_store = ProcessDefStore::new();
        def_store.register(minimal);
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
                process_id: "minimal".into(),
                instance_id: instance_id.clone(),
                initial_variables: None,
            }),
            &mut ctx,
        );
        let inst = repo.load(&instance_id).expect("instance should exist");
        assert!(inst.completed(), "Start->End should complete instance");
    }
}
