//! ProcessService / TaskService (design: overview §6.1, §6.2).
//! Application-layer facades that delegate to BpmEngine and repos.
//! API spec: deploy, start with variables, get instance, cancel, retry token.

use std::collections::HashMap;

use crate::engine::{payloads, BpmEngine, EngineContext, EngineEvent};
use crate::model::{InstanceState, ProcessInstance, TokenStatus};

/// Process control API (design: overview §6.1).
/// start_process / get_process delegate to engine and ProcessInstanceRepo.
pub struct ProcessService;

impl ProcessService {
    /// Start a new process instance. If instance_id is None, generates a new UUID.
    /// Calls engine.run(ProcessStarted(...), ctx). Supports initial variables (API spec).
    pub fn start_process(
        process_id: &str,
        instance_id: Option<String>,
        variables: HashMap<String, String>,
        engine: &BpmEngine,
        ctx: &mut EngineContext,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let instance_id = instance_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let initial_variables = if variables.is_empty() {
            None
        } else {
            Some(variables)
        };
        engine.run(
            EngineEvent::ProcessStarted(payloads::ProcessStarted {
                process_id: process_id.to_string(),
                instance_id: instance_id.clone(),
                initial_variables,
            }),
            ctx,
        );
        Ok(instance_id)
    }

    /// Load a process instance by id. Delegates to ctx.process_repo.
    pub fn get_process(
        instance_id: &str,
        ctx: &EngineContext,
    ) -> Option<ProcessInstance> {
        ctx.process_repo.as_ref().and_then(|r| r.load(instance_id))
    }

    /// Cancel (terminate) an instance. API spec: POST /process-instances/:id/cancel.
    pub fn cancel_instance(
        instance_id: &str,
        ctx: &mut EngineContext,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(process_repo) = ctx.process_repo.as_ref() else {
            return Err("process repo not configured".into());
        };
        let Some(mut instance) = process_repo.load(instance_id) else {
            return Err("instance not found".into());
        };
        if instance.state != InstanceState::Running {
            return Err("instance not running".into());
        }
        instance.state = InstanceState::Terminated;
        process_repo.save(&instance);
        Ok(())
    }

    /// Retry a token: set status to Ready, optionally increment attempt, enqueue TokenArrived and run engine.
    /// API spec: POST /tokens/:token_id/retry.
    pub fn retry_token(
        token_id: &str,
        engine: &BpmEngine,
        ctx: &mut EngineContext,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let process_repo = ctx
            .process_repo
            .as_ref()
            .ok_or("process repo not configured")?;
        let token_repo = ctx
            .token_repo
            .as_ref()
            .ok_or("token repo not configured")?;
        let running_ids = process_repo.list_running(ctx.tenant_id.as_deref());
        for instance_id in running_ids {
            let instance = process_repo
                .load(&instance_id)
                .ok_or("instance not found")?;
            let token = instance
                .tokens
                .iter()
                .find(|t| t.id == token_id)
                .filter(|t| t.status == TokenStatus::Executing || t.status == TokenStatus::Waiting)
                .cloned();
            if let Some(t) = token {
                let reset = crate::model::Token {
                    id: t.id.clone(),
                    node_id: t.node_id.clone(),
                    status: crate::model::TokenStatus::Ready,
                    mode: t.mode,
                    version: t.version,
                    attempt: t.attempt.saturating_add(1),
                    parallel_group_id: t.parallel_group_id.clone(),
                    updated_at: None,
                };
                if !token_repo.update_token_cas(&instance_id, &reset) {
                    return Err("token update conflict".into());
                }
                engine.run(
                    EngineEvent::TokenArrived(payloads::TokenArrived {
                        instance_id: instance_id.clone(),
                        token_id: t.id.clone(),
                        node_id: t.node_id.clone(),
                    }),
                    ctx,
                );
                return Ok(());
            }
        }
        Err("token not found or not retriable".into())
    }
}

/// Task list item for API spec GET /tasks.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskListItem {
    pub task_id: String,
    pub node_id: String,
    pub instance_id: String,
    /// "user" | "external"
    pub task_type: String,
}

/// Task completion API (design: overview §6.2).
/// complete_task delegates to engine.run(UserTaskCompleted(...), ctx).
pub struct TaskService;

impl TaskService {
    /// List pending tasks (tokens in Waiting at UserTask or ServiceTask). API spec: GET /tasks?type=user|external.
    pub fn list_tasks(
        ctx: &EngineContext,
        type_filter: Option<&str>,
    ) -> Vec<TaskListItem> {
        let Some(process_repo) = ctx.process_repo.as_ref() else {
            return vec![];
        };
        let Some(process_def_repo) = ctx.process_def_repo.as_ref() else {
            return vec![];
        };
        let mut out = Vec::new();
        for instance_id in process_repo.list_running(ctx.tenant_id.as_deref()) {
            let Some(instance) = process_repo.load(&instance_id) else {
                continue;
            };
            let Some(def) = process_def_repo.load(&instance.process_def_id) else {
                continue;
            };
            for token in &instance.tokens {
                if token.status != TokenStatus::Waiting {
                    continue;
                }
                let Some(node) = def.nodes.get(token.node_id.as_str()) else {
                    continue;
                };
                let task_type = match &node.node_type {
                    crate::model::NodeType::UserTask => "user",
                    crate::model::NodeType::ServiceTask(_) => "external",
                    _ => continue,
                };
                if let Some(filter) = type_filter {
                    if filter != task_type {
                        continue;
                    }
                }
                let task_id = format!("{}:{}", instance.id, token.node_id);
                out.push(TaskListItem {
                    task_id,
                    node_id: token.node_id.clone(),
                    instance_id: instance.id.clone(),
                    task_type: task_type.to_string(),
                });
            }
        }
        out
    }

    /// Complete a user task. Calls engine.run(UserTaskCompleted(...), ctx).
    pub fn complete_task(
        instance_id: &str,
        node_id: &str,
        task_id: &str,
        variables: HashMap<String, String>,
        engine: &BpmEngine,
        ctx: &mut EngineContext,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        engine.run(
            EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
                task_id: task_id.to_string(),
                instance_id: instance_id.to_string(),
                node_id: node_id.to_string(),
                variables,
            }),
            ctx,
        );
        Ok(())
    }
}
