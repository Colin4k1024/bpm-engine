//! REST API v1 routes: process-instances, tasks.

use bpm_core::{payloads, EngineEvent, InstanceState, NodeType, ProcessInstance, TokenStatus};
use bpm_runtime::EngineContext;
use bpm_storage::{
    CompensationRecordRepo, ParallelJoinRepo, ProcessDefinitionRepo, ProcessInstanceRepo,
    TimerRepo, TokenRepo,
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::state::AppState;

fn tenant_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

fn status_str(state: InstanceState) -> &'static str {
    match state {
        InstanceState::Running => "RUNNING",
        InstanceState::Completed => "COMPLETED",
        InstanceState::Terminated => "TERMINATED",
    }
}

fn current_nodes(inst: &ProcessInstance) -> Vec<String> {
    let mut nodes: Vec<String> = inst
        .tokens
        .iter()
        .filter(|t| t.status == TokenStatus::Waiting)
        .map(|t| t.node_id.clone())
        .collect();
    nodes.sort();
    nodes.dedup();
    nodes
}

fn build_ctx(state: &AppState, tenant_id: Option<String>) -> EngineContext {
    let repo = Arc::clone(&state.repo);
    let def_store = Arc::clone(&state.def_store);
    EngineContext {
        process_repo: Some(repo.clone() as Arc<dyn ProcessInstanceRepo>),
        token_repo: Some(repo.clone() as Arc<dyn TokenRepo>),
        process_def_repo: Some(def_store.clone() as Arc<dyn ProcessDefinitionRepo>),
        parallel_join_repo: Some(repo.clone() as Arc<dyn ParallelJoinRepo>),
        timer_repo: Some(repo.clone() as Arc<dyn TimerRepo>),
        compensation_repo: Some(repo as Arc<dyn CompensationRecordRepo>),
        outbox_repo: None,
        tenant_id,
    }
}

#[derive(Deserialize)]
pub struct StartInstanceRequest {
    pub process_def_id: String,
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct StartInstanceResponse {
    pub instance_id: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct InstanceStateResponse {
    pub instance_id: String,
    pub status: String,
    pub current_nodes: Vec<String>,
}

#[derive(Deserialize)]
pub struct CompleteTaskBodyRequest {
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct CompleteTaskResponse {
    pub status: String,
}

#[derive(Serialize)]
pub struct TaskListItem {
    pub task_id: String,
    pub node_id: String,
    pub instance_id: String,
    pub task_type: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// POST /api/v1/process-instances — start a process instance.
pub async fn start_instance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<StartInstanceRequest>,
) -> Result<(StatusCode, Json<StartInstanceResponse>), (StatusCode, Json<ErrorResponse>)> {
    let tenant_id = tenant_from_headers(&headers);
    let mut ctx = build_ctx(state.as_ref(), tenant_id);
    let instance_id = uuid::Uuid::new_v4().to_string();
    let initial_variables = if body.variables.is_empty() {
        None
    } else {
        Some(body.variables)
    };
    let ev = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: body.process_def_id.clone(),
        instance_id: instance_id.clone(),
        initial_variables,
    });
    state.engine.run_async(ev, &mut ctx).await;
    Ok((
        StatusCode::CREATED,
        Json(StartInstanceResponse {
            instance_id,
            status: "RUNNING".to_string(),
        }),
    ))
}

/// GET /api/v1/process-instances/:id
pub async fn get_instance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<InstanceStateResponse>, (StatusCode, Json<ErrorResponse>)> {
    let tenant_id = tenant_from_headers(&headers);
    let _ctx = build_ctx(state.as_ref(), tenant_id);
    let repo = Arc::clone(&state.repo);
    let inst = repo.load(&id).await.ok().flatten();
    match inst {
        Some(inst) => Ok(Json(InstanceStateResponse {
            instance_id: inst.id.clone(),
            status: status_str(inst.state).to_string(),
            current_nodes: current_nodes(&inst),
        })),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("process instance not found: {}", id),
            }),
        )),
    }
}

/// GET /api/v1/tasks?type=user|external
pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Vec<TaskListItem>> {
    let tenant_id = tenant_from_headers(&headers);
    let repo = Arc::clone(&state.repo);
    let def_store = Arc::clone(&state.def_store);
    let running_ids = repo.list_running(tenant_id.as_deref()).await.unwrap_or_default();
    let type_filter = params.get("type").map(String::as_str);
    let mut out = Vec::new();
    for instance_id in running_ids {
        let Ok(Some(instance)) = repo.load(&instance_id).await else {
            continue;
        };
        let Ok(Some(def)) = def_store.load(&instance.process_def_id).await else {
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
                NodeType::UserTask => "user",
                NodeType::ServiceTask(_) => "external",
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
    Json(out)
}

fn parse_task_id(task_id: &str) -> Option<(String, String)> {
    let mut parts = task_id.rsplitn(2, ':');
    let node_id = parts.next()?.to_string();
    let instance_id = parts.next()?.to_string();
    if instance_id.is_empty() || node_id.is_empty() {
        return None;
    }
    Some((instance_id, node_id))
}

/// POST /api/v1/tasks/:task_id/complete
pub async fn complete_task_by_id(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Json(body): Json<CompleteTaskBodyRequest>,
) -> Result<Json<CompleteTaskResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (instance_id, node_id) = parse_task_id(&task_id).ok_or((
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: format!("invalid task_id: {}", task_id),
        }),
    ))?;
    let tenant_id = tenant_from_headers(&headers);
    let mut ctx = build_ctx(state.as_ref(), tenant_id);
    let ev = EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
        task_id: task_id.clone(),
        instance_id: instance_id.clone(),
        node_id: node_id.clone(),
        variables: body.variables,
    });
    state.engine.run_async(ev, &mut ctx).await;
    Ok(Json(CompleteTaskResponse {
        status: "COMPLETED".to_string(),
    }))
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .nest(
            "/api/v1",
            Router::new()
                .route("/process-instances", post(start_instance))
                .route("/process-instances/:id", get(get_instance))
                .route("/tasks", get(list_tasks))
                .route("/tasks/:task_id/complete", post(complete_task_by_id))
                .with_state(state),
        )
}
