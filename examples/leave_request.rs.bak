//! 请假流程示例：多级 EL 网关 + 人工审核
//!
//! Run: `cargo run --example leave_request`
//!
//! 流程：Start → 提交请假(设置变量) → 路由网关(EL) → 自动通过 / 经理审批 / 总监审批 → 结果网关 → 通过/驳回
//!
//! EL 路由规则（按顺序匹配）：
//! - days > 5 → 总监审批
//! - leave_type == "sick" → 经理审批（病假一律经理）
//! - days > 2 → 经理审批
//! - Default → 自动通过

use bpm_engine::engine::{
    payloads, BpmEngine, EngineContext, EngineEvent, ProcessCompletedHandler, ProcessStartHandler,
    TokenArrivedHandler, UserTaskCompletedHandler,
};
use bpm_engine::model::*;
use bpm_engine::persistence::{InstanceRepo, ProcessDefStore, ProcessInstanceRepo};
use std::collections::HashMap;
use std::sync::Arc;

/// 模拟员工提交请假单：设置 days, leave_type, reason
fn submit_leave(instance: &mut ProcessInstance) {
    // 可改为从外部传入；这里写死为 4 天年假，走「经理审批」分支
    instance.variables.insert("days".into(), "4".into());
    instance.variables.insert("leave_type".into(), "annual".into());
    instance.variables.insert("reason".into(), "family".into());
    println!("  [提交请假] days=4, leave_type=annual, reason=family");
}

/// 自动通过（短假/默认分支）
fn auto_approve(instance: &mut ProcessInstance) {
    instance.variables.insert("approved".into(), "true".into());
    println!("  [自动通过] 无需审批");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process = ProcessDefinition {
        id: "leave_request",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "submit_leave",
                        condition: None,
                    }],
                },
            ),
            (
                "submit_leave",
                Node {
                    id: "submit_leave",
                    node_type: NodeType::ServiceTask(submit_leave),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "gateway_route",
                        condition: None,
                    }],
                },
            ),
            // 路由网关：按 EL 顺序匹配 → 总监 / 经理 / 自动通过
            (
                "gateway_route",
                Node {
                    id: "gateway_route",
                    node_type: NodeType::ExclusiveGateway,
                    outgoing_edges: vec![
                        OutgoingEdge {
                            target: "director_approve",
                            condition: Some(EdgeCondition::Expression("days > 5".into())),
                        },
                        OutgoingEdge {
                            target: "manager_approve",
                            condition: Some(EdgeCondition::Expression(r#"leave_type == "sick""#.into())),
                        },
                        OutgoingEdge {
                            target: "manager_approve",
                            condition: Some(EdgeCondition::Expression("days > 2".into())),
                        },
                        OutgoingEdge {
                            target: "auto_approve",
                            condition: Some(EdgeCondition::Default),
                        },
                    ],
                },
            ),
            (
                "auto_approve",
                Node {
                    id: "auto_approve",
                    node_type: NodeType::ServiceTask(auto_approve),
                    outgoing_edges: vec![OutgoingEdge {
                        target: "end_approved",
                        condition: None,
                    }],
                },
            ),
            (
                "manager_approve",
                Node {
                    id: "manager_approve",
                    node_type: NodeType::UserTask,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "gateway_result",
                        condition: None,
                    }],
                },
            ),
            (
                "director_approve",
                Node {
                    id: "director_approve",
                    node_type: NodeType::UserTask,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "gateway_result",
                        condition: None,
                    }],
                },
            ),
            // 结果网关：审批通过 / 驳回
            (
                "gateway_result",
                Node {
                    id: "gateway_result",
                    node_type: NodeType::ExclusiveGateway,
                    outgoing_edges: vec![
                        OutgoingEdge {
                            target: "end_approved",
                            condition: Some(EdgeCondition::Expression(r#"approved == "true""#.into())),
                        },
                        OutgoingEdge {
                            target: "end_rejected",
                            condition: Some(EdgeCondition::Default),
                        },
                    ],
                },
            ),
            (
                "end_approved",
                Node {
                    id: "end_approved",
                    node_type: NodeType::End,
                    outgoing_edges: vec![],
                },
            ),
            (
                "end_rejected",
                Node {
                    id: "end_rejected",
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
    println!("--- 启动请假流程 (days=4 年假 → 经理审批) ---");
    engine.run(
        EngineEvent::ProcessStarted(payloads::ProcessStarted {
            process_id: process.id.to_string(),
            instance_id: instance_id.clone(),
            initial_variables: None,
        }),
        &mut ctx,
    );
    println!("  流程暂停于 UserTask「经理审批」\n");

    // 模拟经理审批通过
    println!("--- 经理审批：通过 ---");
    engine.run(
        EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
            task_id: String::new(),
            instance_id: instance_id.clone(),
            node_id: "manager_approve".into(),
            variables: HashMap::from([("approved".into(), "true".into())]),
        }),
        &mut ctx,
    );

    let inst = repo.load(&instance_id).expect("instance exists");
    assert!(inst.completed());
    println!("\nOK: 流程已结束，请假已通过 (end_approved)。");
    Ok(())
}
