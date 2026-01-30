//! ProcessService / TaskService (design: overview §6.1, §6.2).
//! Application-layer facades that delegate to BpmEngine and repos.

use std::collections::HashMap;

use crate::engine::{payloads, BpmEngine, EngineContext, EngineEvent};
use crate::model::ProcessInstance;

/// Process control API (design: overview §6.1).
/// start_process / get_process delegate to engine and ProcessInstanceRepo.
pub struct ProcessService;

impl ProcessService {
    /// Start a new process instance. If instance_id is None, generates a new UUID.
    /// Calls engine.run(ProcessStarted(...), ctx).
    pub fn start_process(
        process_id: &str,
        instance_id: Option<String>,
        engine: &BpmEngine,
        ctx: &mut EngineContext,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let instance_id = instance_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        engine.run(
            EngineEvent::ProcessStarted(payloads::ProcessStarted {
                process_id: process_id.to_string(),
                instance_id: instance_id.clone(),
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
}

/// Task completion API (design: overview §6.2).
/// complete_task delegates to engine.run(UserTaskCompleted(...), ctx).
pub struct TaskService;

impl TaskService {
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
