//! REST API v1 routes: process-instances, tasks, external-tasks.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use bpm_engine_core::{
    payloads, EngineEvent, ExternalTaskState, InstanceState, NodeType, ProcessDefinition,
    ProcessInstance, TokenStatus,
};
use bpm_engine_runtime::{transition, EngineContext};
use bpm_engine_storage::{
    CompensationRecordRepo, ExternalTaskStore, HistoryRepo, ParallelJoinRepo,
    ProcessDefinitionStore, ProcessInstanceStore, TimerStore, TokenStore,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::state::AppState;

// --- External task API (plan §6) ---

#[derive(Deserialize)]
pub struct FetchAndLockRequest {
    pub worker_id: String,
    pub task_types: Vec<String>,
    #[serde(default = "default_max_tasks")]
    pub max_tasks: u32,
    pub lock_duration_ms: u64,
}

fn default_max_tasks() -> u32 {
    10
}

#[derive(Serialize)]
pub struct ExternalTaskResponse {
    pub task_id: String,
    pub token_id: String,
    pub process_instance_id: String,
    pub task_type: String,
    pub variables: HashMap<String, String>,
}

#[derive(Deserialize)]
pub struct ExternalTaskCompleteRequest {
    pub worker_id: String,
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Deserialize)]
pub struct ExternalTaskFailRequest {
    pub worker_id: String,
    pub error: String,
    pub retry_after_ms: Option<u64>,
}

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
        process_store: Some(repo.clone() as Arc<dyn ProcessInstanceStore>),
        token_store: Some(repo.clone() as Arc<dyn TokenStore>),
        process_def_store: Some(def_store.clone() as Arc<dyn ProcessDefinitionStore>),
        parallel_join_repo: Some(repo.clone() as Arc<dyn ParallelJoinRepo>),
        timer_store: Some(repo.clone() as Arc<dyn TimerStore>),
        compensation_repo: Some(repo.clone() as Arc<dyn CompensationRecordRepo>),
        outbox_repo: None,
        external_task_store: Some(repo.clone() as Arc<dyn ExternalTaskStore>),
        history_repo: Some(repo.clone() as Arc<dyn HistoryRepo>),
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
    pub process_def_id: String,
    pub status: String,
    pub current_nodes: Vec<String>,
    pub tokens: Vec<bpm_engine_core::Token>,
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

#[derive(Serialize)]
pub struct DeployResponse {
    pub process_definition_id: String,
}

/// Deploy failure: either parse error (single message) or compile errors (list).
#[derive(Serialize)]
#[serde(untagged)]
pub enum DeployErrorResponse {
    Parse {
        error: String,
    },
    Compile {
        errors: Vec<bpm_engine_bpmn::CompilerError>,
    },
}

// --- Process definition view (Trace UI diagram) ---

#[derive(Serialize)]
pub struct NodeView {
    pub id: String,
    pub node_type: String,
}

#[derive(Serialize)]
pub struct EdgeView {
    pub source: String,
    pub target: String,
}

#[derive(Serialize)]
pub struct ProcessDefinitionView {
    pub id: String,
    pub start: String,
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
}

fn node_type_str(nt: &NodeType) -> &'static str {
    match nt {
        NodeType::Start => "Start",
        NodeType::End => "End",
        NodeType::ServiceTask(_) => "ServiceTask",
        NodeType::UserTask => "UserTask",
        NodeType::ExternalTask { .. } => "ExternalTask",
        NodeType::ExclusiveGateway => "ExclusiveGateway",
        NodeType::ParallelFork => "ParallelFork",
        NodeType::ParallelJoin { .. } => "ParallelJoin",
    }
}

fn process_definition_to_view(def: &ProcessDefinition) -> ProcessDefinitionView {
    let mut nodes = Vec::with_capacity(def.nodes.len());
    let mut edges = Vec::new();
    for (id, node) in &def.nodes {
        nodes.push(NodeView {
            id: id.to_string(),
            node_type: node_type_str(&node.node_type).to_string(),
        });
        for out in &node.outgoing_edges {
            edges.push(EdgeView {
                source: id.to_string(),
                target: out.target.to_string(),
            });
        }
    }
    ProcessDefinitionView {
        id: def.id.to_string(),
        start: def.start.to_string(),
        nodes,
        edges,
    }
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
            process_def_id: inst.process_def_id.clone(),
            status: status_str(inst.state).to_string(),
            current_nodes: current_nodes(&inst),
            tokens: inst.tokens.clone(),
        })),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("process instance not found: {}", id),
            }),
        )),
    }
}

/// GET /api/v1/process-definitions/:id — process definition view for Trace UI diagram.
pub async fn get_process_definition(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ProcessDefinitionView>, (StatusCode, Json<ErrorResponse>)> {
    let def = state.def_store.load(&id).await.ok().flatten().ok_or((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: format!("process definition not found: {}", id),
        }),
    ))?;
    Ok(Json(process_definition_to_view(&def)))
}

/// GET /api/v1/process-instances/:id/history — execution history for Trace UI timeline.
pub async fn get_instance_history(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<bpm_engine_storage::HistoryEvent>>, (StatusCode, Json<ErrorResponse>)> {
    let token_id = params.get("token_id").map(String::as_str);
    let event_type = params.get("event_type").map(String::as_str);
    let events = HistoryRepo::list_by_instance(state.repo.as_ref(), &id, token_id, event_type)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    Ok(Json(events))
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
    let running_ids = repo
        .list_running(tenant_id.as_deref())
        .await
        .unwrap_or_default();
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
                NodeType::ServiceTask(_) | NodeType::ExternalTask { .. } => "external",
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

/// POST /api/v1/external-tasks/fetch-and-lock
pub async fn external_task_fetch_and_lock(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FetchAndLockRequest>,
) -> Result<Json<Vec<ExternalTaskResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let repo = Arc::clone(&state.repo);
    let _ = repo.reclaim_expired_locks().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;
    let lock_duration = Duration::from_millis(body.lock_duration_ms);
    let max_tasks = body.max_tasks as usize;
    let tasks = repo
        .fetch_and_lock(&body.worker_id, &body.task_types, max_tasks, lock_duration)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    let out: Vec<ExternalTaskResponse> = tasks
        .into_iter()
        .map(|t| ExternalTaskResponse {
            task_id: t.task_id,
            token_id: t.token_id,
            process_instance_id: t.process_instance_id,
            task_type: t.task_type,
            variables: t.variables,
        })
        .collect();
    Ok(Json(out))
}

/// POST /api/v1/external-tasks/:task_id/complete
pub async fn external_task_complete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Json(body): Json<ExternalTaskCompleteRequest>,
) -> Result<Json<CompleteTaskResponse>, (StatusCode, Json<ErrorResponse>)> {
    let repo = Arc::clone(&state.repo);
    repo.complete(&task_id, &body.worker_id, body.variables.clone())
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    let task = repo
        .get(&task_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("task not found after complete: {}", task_id),
            }),
        ))?;
    let tenant_id = tenant_from_headers(&headers);
    let mut ctx = build_ctx(state.as_ref(), tenant_id);
    let process_store = ctx.process_store.as_ref().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "process_store not configured".to_string(),
        }),
    ))?;
    let process_def_store = ctx.process_def_store.as_ref().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "process_def_store not configured".to_string(),
        }),
    ))?;
    let mut instance = process_store
        .load(&task.process_instance_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("instance not found: {}", task.process_instance_id),
            }),
        ))?;
    let token = instance
        .tokens
        .iter()
        .find(|t| t.id == task.token_id)
        .cloned();
    let node_id = token.as_ref().map(|t| t.node_id.clone()).ok_or((
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: format!("token not found in instance: {}", task.token_id),
        }),
    ))?;
    let def = process_def_store
        .load(&instance.process_def_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("process def not found: {}", instance.process_def_id),
            }),
        ))?;
    let node = def.nodes.get(node_id.as_str()).ok_or((
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: format!("node not found: {}", node_id),
        }),
    ))?;
    instance.tokens.retain(|t| t.id != task.token_id);
    let new_tokens = transition::move_token(node);
    for (k, v) in body.variables {
        instance.variables.insert(k, v);
    }
    instance.tokens.extend(new_tokens.clone());
    process_store.save(&instance).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;
    for t in &new_tokens {
        let ev = EngineEvent::TokenArrived(payloads::TokenArrived {
            instance_id: task.process_instance_id.clone(),
            token_id: t.id.clone(),
            node_id: t.node_id.clone(),
        });
        state.engine.run_async(ev, &mut ctx).await;
    }
    Ok(Json(CompleteTaskResponse {
        status: "COMPLETED".to_string(),
    }))
}

/// POST /api/v1/external-tasks/:task_id/fail
pub async fn external_task_fail(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
    Json(body): Json<ExternalTaskFailRequest>,
) -> Result<Json<CompleteTaskResponse>, (StatusCode, Json<ErrorResponse>)> {
    let repo = Arc::clone(&state.repo);
    let retry_after = body.retry_after_ms.map(Duration::from_millis);
    repo.fail(&task_id, &body.worker_id, body.error.clone(), retry_after)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    let task = repo
        .get(&task_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("task not found after fail: {}", task_id),
            }),
        ))?;
    if task.state == ExternalTaskState::Failed {
        let tenant_id = None::<String>;
        let mut ctx = build_ctx(state.as_ref(), tenant_id);
        let inst = state
            .repo
            .load(&task.process_instance_id)
            .await
            .ok()
            .flatten();
        let node_id = inst
            .as_ref()
            .and_then(|i| {
                i.tokens
                    .iter()
                    .find(|t| t.id == task.token_id)
                    .map(|t| t.node_id.clone())
            })
            .unwrap_or_default();
        let ev = EngineEvent::TokenFailed(payloads::TokenFailed {
            instance_id: task.process_instance_id.clone(),
            token_id: task.token_id.clone(),
            node_id,
            reason: task.error_message.unwrap_or(body.error),
        });
        state.engine.run_async(ev, &mut ctx).await;
    }
    Ok(Json(CompleteTaskResponse {
        status: "FAILED".to_string(),
    }))
}

/// POST /api/v1/process-definitions/deploy — deploy a process definition from BPMN 2.0 XML.
/// On compile failure returns 400 with list of CompilerErrors (03.md).
pub async fn deploy_bpmn(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<(StatusCode, Json<DeployResponse>), (StatusCode, Json<DeployErrorResponse>)> {
    let def = match bpm_engine_bpmn::parse_and_compile(&body) {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(match e {
                    bpm_engine_bpmn::CompileError::Parse(parse_err) => DeployErrorResponse::Parse {
                        error: parse_err.to_string(),
                    },
                    bpm_engine_bpmn::CompileError::Compile(ce) => {
                        DeployErrorResponse::Compile { errors: ce.0 }
                    }
                }),
            ))
        }
    };
    let process_definition_id = def.id.to_string();
    state.def_store.register(def);
    Ok((
        StatusCode::CREATED,
        Json(DeployResponse {
            process_definition_id,
        }),
    ))
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new().nest(
        "/api/v1",
        Router::new()
            .route("/process-instances", post(start_instance))
            .route("/process-instances/:id", get(get_instance))
            .route("/process-instances/:id/history", get(get_instance_history))
            .route("/process-definitions/:id", get(get_process_definition))
            .route("/tasks", get(list_tasks))
            .route("/tasks/:task_id/complete", post(complete_task_by_id))
            .route(
                "/external-tasks/fetch-and-lock",
                post(external_task_fetch_and_lock),
            )
            .route(
                "/external-tasks/:task_id/complete",
                post(external_task_complete),
            )
            .route("/external-tasks/:task_id/fail", post(external_task_fail))
            .route("/process-definitions/deploy", post(deploy_bpmn))
            .with_state(state),
    )
}
