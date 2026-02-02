//! REST API v1 routes: process-instances, tasks, external-tasks.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
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
use std::time::{SystemTime, UNIX_EPOCH};

fn occurred_at_now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string()
}

use super::replay::ReplaySession;
use super::state::AppState;
use bpm_engine_adapter_memory::MemoryRepo;

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
    build_ctx_with_repo(Arc::clone(&state.repo), &state.def_store, tenant_id)
}

/// Build engine context with a specific repo (e.g. replay temporary repo).
fn build_ctx_with_repo(
    repo: Arc<MemoryRepo>,
    def_store: &Arc<bpm_engine_adapter_memory::ProcessDefStore>,
    tenant_id: Option<String>,
) -> EngineContext {
    let def_store = Arc::clone(def_store);
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

fn instance_id_from_event(ev: &EngineEvent) -> Option<String> {
    use bpm_engine_core::EngineEvent;
    match ev {
        EngineEvent::ProcessStarted(p) => Some(p.instance_id.clone()),
        EngineEvent::TokenArrived(p) => Some(p.instance_id.clone()),
        EngineEvent::TokenCompleted(p) => Some(p.instance_id.clone()),
        EngineEvent::UserTaskCreated(p) => Some(p.instance_id.clone()),
        EngineEvent::UserTaskCompleted(p) => Some(p.instance_id.clone()),
        EngineEvent::TimerFired(_) => None,
        EngineEvent::TimerScheduled(p) => Some(p.instance_id.clone()),
        EngineEvent::TokenFailed(p) => Some(p.instance_id.clone()),
        EngineEvent::SagaStarted(p) => Some(p.instance_id.clone()),
        EngineEvent::SagaCompleted(p) => Some(p.instance_id.clone()),
        EngineEvent::ProcessCompleted(p) => Some(p.instance_id.clone()),
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

// --- Trace API (aggregated view) ---

#[derive(Serialize)]
pub struct TraceEventView {
    pub event_type: String,
    pub occurred_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct TokenTimelineView {
    pub token_id: String,
    pub node_id: String,
    pub status: String,
    pub events: Vec<TraceEventView>,
}

#[derive(Serialize)]
pub struct ExternalTaskHistoryEntryView {
    pub task_id: String,
    pub token_id: String,
    pub process_instance_id: String,
    pub events: Vec<TraceEventView>,
}

#[derive(Serialize)]
pub struct TraceResponse {
    pub instance: InstanceStateResponse,
    pub token_timelines: Vec<TokenTimelineView>,
    pub external_task_history: Vec<ExternalTaskHistoryEntryView>,
}

fn token_status_str(status: &bpm_engine_core::TokenStatus) -> &'static str {
    use bpm_engine_core::TokenStatus;
    match status {
        TokenStatus::Created => "CREATED",
        TokenStatus::Ready => "READY",
        TokenStatus::Executing => "EXECUTING",
        TokenStatus::Waiting => "WAITING",
        TokenStatus::Suspended => "SUSPENDED",
        TokenStatus::Completed => "COMPLETED",
        TokenStatus::Terminated => "TERMINATED",
    }
}

/// GET /api/v1/process-instances/:id/trace — aggregated execution trace (instance + token timelines + external task history).
pub async fn get_instance_trace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<TraceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let repo = Arc::clone(&state.repo);
    let inst = repo.load(&id).await.ok().flatten().ok_or((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: format!("process instance not found: {}", id),
        }),
    ))?;
    let instance_response = InstanceStateResponse {
        instance_id: inst.id.clone(),
        process_def_id: inst.process_def_id.clone(),
        status: status_str(inst.state).to_string(),
        current_nodes: current_nodes(&inst),
        tokens: inst.tokens.clone(),
    };
    let events = HistoryRepo::list_by_instance(state.repo.as_ref(), &id, None, None)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    let token_ids_in_instance: std::collections::HashMap<
        String,
        (String, bpm_engine_core::TokenStatus),
    > = inst
        .tokens
        .iter()
        .map(|t| (t.id.clone(), (t.node_id.clone(), t.status)))
        .collect();
    let mut by_token: std::collections::HashMap<String, Vec<TraceEventView>> =
        std::collections::HashMap::new();
    for ev in &events {
        let token_id = ev
            .payload
            .get("token_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let Some(token_id) = token_id else {
            continue;
        };
        if token_id.is_empty() {
            continue;
        }
        let entry = by_token.entry(token_id).or_default();
        entry.push(TraceEventView {
            event_type: ev.event_type.clone(),
            occurred_at: ev.occurred_at.clone(),
            payload: Some(ev.payload.clone()),
        });
    }
    let mut token_timelines: Vec<TokenTimelineView> = by_token
        .into_iter()
        .map(|(token_id, evs)| {
            let (node_id, status) = token_ids_in_instance
                .get(&token_id)
                .map(|(n, s)| (n.clone(), token_status_str(s).to_string()))
                .unwrap_or_else(|| {
                    let node_id = evs
                        .last()
                        .and_then(|e| e.payload.as_ref())
                        .and_then(|p| p.get("node_id").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let status = evs
                        .last()
                        .map(|e| e.event_type.as_str())
                        .map(|t| match t {
                            "TokenCompleted" => "COMPLETED",
                            "TokenFailed" => "TERMINATED",
                            _ => "UNKNOWN",
                        })
                        .unwrap_or("UNKNOWN")
                        .to_string();
                    (node_id, status)
                });
            TokenTimelineView {
                token_id,
                node_id,
                status,
                events: evs,
            }
        })
        .collect();
    token_timelines.sort_by(|a, b| {
        a.events
            .first()
            .zip(b.events.first())
            .map(|(ea, eb)| ea.occurred_at.cmp(&eb.occurred_at))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let ext_events: Vec<&bpm_engine_storage::HistoryEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e.event_type.as_str(),
                "ExternalTaskLocked" | "ExternalTaskFailed" | "ExternalTaskCompleted"
            )
        })
        .collect();
    let mut by_task: std::collections::HashMap<String, Vec<TraceEventView>> =
        std::collections::HashMap::new();
    for ev in ext_events {
        let task_id = ev
            .payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| ev.id.clone());
        let entry = by_task.entry(task_id).or_default();
        entry.push(TraceEventView {
            event_type: ev.event_type.clone(),
            occurred_at: ev.occurred_at.clone(),
            payload: Some(ev.payload.clone()),
        });
    }
    let external_task_history: Vec<ExternalTaskHistoryEntryView> = by_task
        .into_iter()
        .map(|(task_id, evs)| {
            let first = evs.first().and_then(|e| e.payload.as_ref());
            let token_id = first
                .and_then(|p| p.get("token_id").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let process_instance_id = first
                .and_then(|p| p.get("process_instance_id").and_then(|v| v.as_str()))
                .unwrap_or(id.as_str())
                .to_string();
            ExternalTaskHistoryEntryView {
                task_id,
                token_id,
                process_instance_id,
                events: evs,
            }
        })
        .collect();
    Ok(Json(TraceResponse {
        instance: instance_response,
        token_timelines,
        external_task_history,
    }))
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
    let occurred_at = occurred_at_now();
    for t in &tasks {
        let payload = serde_json::json!({
            "task_id": t.task_id,
            "token_id": t.token_id,
            "process_instance_id": t.process_instance_id,
            "worker_id": body.worker_id,
            "retries": t.retries,
        });
        let _ = HistoryRepo::append(
            state.repo.as_ref(),
            &t.process_instance_id,
            "ExternalTaskLocked",
            &payload,
            &occurred_at,
        )
        .await;
    }
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
    let occurred_at = occurred_at_now();
    let payload = serde_json::json!({
        "task_id": task.task_id,
        "token_id": task.token_id,
        "process_instance_id": task.process_instance_id,
        "worker_id": body.worker_id,
    });
    let _ = HistoryRepo::append(
        state.repo.as_ref(),
        &task.process_instance_id,
        "ExternalTaskCompleted",
        &payload,
        &occurred_at,
    )
    .await;
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
    let occurred_at = occurred_at_now();
    let payload = serde_json::json!({
        "task_id": task.task_id,
        "token_id": task.token_id,
        "process_instance_id": task.process_instance_id,
        "worker_id": body.worker_id,
        "retries": task.retries,
        "error_message": body.error,
    });
    let _ = HistoryRepo::append(
        state.repo.as_ref(),
        &task.process_instance_id,
        "ExternalTaskFailed",
        &payload,
        &occurred_at,
    )
    .await;
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

// --- Replay API (docs_replay_rest_api.md) ---

#[derive(Serialize)]
pub struct ReplayCreateResponse {
    pub session_id: String,
    pub instance_id: String,
    pub total_events: usize,
}

#[derive(Serialize)]
pub struct ReplayEventView {
    pub event_type: String,
    pub occurred_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

#[derive(Serialize)]
pub struct ReplayTokenView {
    pub token_id: String,
    pub node_id: String,
    pub state: String,
}

#[derive(Serialize)]
pub struct ReplaySnapshotView {
    pub completed: bool,
    pub tokens: Vec<ReplayTokenView>,
}

#[derive(Serialize)]
pub struct ReplayStepResponse {
    pub cursor: usize,
    pub event: ReplayEventView,
    pub snapshot: ReplaySnapshotView,
}

#[derive(Deserialize)]
pub struct ReplaySeekRequest {
    pub cursor: usize,
}

#[derive(Serialize)]
pub struct ReplaySeekResponse {
    pub cursor: usize,
    pub snapshot: ReplaySnapshotView,
}

#[derive(Serialize)]
pub struct ReplaySnapshotResponse {
    pub cursor: usize,
    pub total_events: usize,
    pub completed: bool,
    pub tokens: Vec<ReplayTokenView>,
}

fn replay_snapshot_from_instance(inst: Option<&ProcessInstance>) -> ReplaySnapshotView {
    let (completed, tokens) = match inst {
        Some(i) => (
            i.state == InstanceState::Completed,
            i.tokens
                .iter()
                .map(|t| ReplayTokenView {
                    token_id: t.id.clone(),
                    node_id: t.node_id.clone(),
                    state: token_status_str(&t.status).to_string(),
                })
                .collect(),
        ),
        None => (false, vec![]),
    };
    ReplaySnapshotView { completed, tokens }
}

/// POST /api/v1/process-instances/:id/replay — create replay session.
pub async fn create_replay(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<ReplayCreateResponse>), (StatusCode, Json<ErrorResponse>)> {
    let _inst = state.repo.load(&id).await.ok().flatten().ok_or((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: format!("process instance not found: {}", id),
        }),
    ))?;
    let events = HistoryRepo::list_by_instance(state.repo.as_ref(), &id, None, None)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let session = ReplaySession::new(id.clone(), events);
    let total_events = session.total_events();
    {
        let mut guard = state.replay_sessions.write().unwrap();
        guard.insert(session_id.clone(), session);
    }
    Ok((
        StatusCode::CREATED,
        Json(ReplayCreateResponse {
            session_id,
            instance_id: id,
            total_events,
        }),
    ))
}

/// Apply one event to a temporary repo and return new snapshot.
async fn replay_apply_one(
    engine: &bpm_engine_runtime::BpmEngine,
    def_store: &Arc<bpm_engine_adapter_memory::ProcessDefStore>,
    snapshot: Option<&ProcessInstance>,
    ev: &bpm_engine_storage::HistoryEvent,
) -> Option<ProcessInstance> {
    let engine_ev = ReplaySession::parse_event(ev)?;
    let instance_id = instance_id_from_event(&engine_ev)?;
    let replay_repo = Arc::new(MemoryRepo::new());
    if let Some(inst) = snapshot {
        let _ = replay_repo.save(inst).await;
    }
    let mut ctx = build_ctx_with_repo(replay_repo.clone(), def_store, None);
    engine.run_async(engine_ev, &mut ctx).await;
    replay_repo.load(&instance_id).await.ok().flatten()
}

/// POST /api/v1/replay/:session_id/step — step forward one event.
pub async fn replay_step(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ReplayStepResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (ev_clone, snapshot_clone) = {
        let guard = state.replay_sessions.read().unwrap();
        let session = guard.get(&session_id).ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "replay session not found or expired".to_string(),
            }),
        ))?;
        let ev = session.current_event().ok_or((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "no more events to step".to_string(),
            }),
        ))?;
        (ev.clone(), session.snapshot.clone())
    };
    let new_snapshot = replay_apply_one(
        &state.engine,
        &state.def_store,
        snapshot_clone.as_ref(),
        &ev_clone,
    )
    .await
    .ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "replay apply failed".to_string(),
        }),
    ))?;
    let token_id = ev_clone
        .payload
        .get("token_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let node_id = ev_clone
        .payload
        .get("node_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let event_view = ReplayEventView {
        event_type: ev_clone.event_type.clone(),
        occurred_at: ev_clone.occurred_at.clone(),
        token_id,
        node_id,
    };
    let cursor = {
        let mut guard = state.replay_sessions.write().unwrap();
        let session = guard.get_mut(&session_id).ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "replay session not found or expired".to_string(),
            }),
        ))?;
        session.snapshot = Some(new_snapshot.clone());
        session.cursor += 1;
        session.cursor
    };
    let snapshot_view = replay_snapshot_from_instance(Some(&new_snapshot));
    Ok(Json(ReplayStepResponse {
        cursor,
        event: event_view,
        snapshot: snapshot_view,
    }))
}

/// POST /api/v1/replay/:session_id/seek — jump to cursor (replay events[0..cursor]).
pub async fn replay_seek(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<ReplaySeekRequest>,
) -> Result<Json<ReplaySeekResponse>, (StatusCode, Json<ErrorResponse>)> {
    let events_to_apply: Vec<bpm_engine_storage::HistoryEvent> = {
        let guard = state.replay_sessions.read().unwrap();
        let session = guard.get(&session_id).ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "replay session not found or expired".to_string(),
            }),
        ))?;
        let cursor = body.cursor.min(session.events.len());
        session.events[..cursor].to_vec()
    };
    let cursor = events_to_apply.len();
    let mut snapshot: Option<ProcessInstance> = None;
    for ev in &events_to_apply {
        if let Some(inst) =
            replay_apply_one(&state.engine, &state.def_store, snapshot.as_ref(), ev).await
        {
            snapshot = Some(inst);
        }
    }
    {
        let mut guard = state.replay_sessions.write().unwrap();
        if let Some(session) = guard.get_mut(&session_id) {
            session.cursor = cursor;
            session.snapshot = snapshot.clone();
        }
    }
    let snapshot_view = replay_snapshot_from_instance(snapshot.as_ref());
    Ok(Json(ReplaySeekResponse {
        cursor,
        snapshot: snapshot_view,
    }))
}

/// GET /api/v1/replay/:session_id/snapshot — read-only snapshot.
pub async fn get_replay_snapshot(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ReplaySnapshotResponse>, (StatusCode, Json<ErrorResponse>)> {
    let guard = state.replay_sessions.read().unwrap();
    let session = guard.get(&session_id).ok_or((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "replay session not found or expired".to_string(),
        }),
    ))?;
    let (completed, tokens) = match &session.snapshot {
        Some(i) => (
            i.state == InstanceState::Completed,
            i.tokens
                .iter()
                .map(|t| ReplayTokenView {
                    token_id: t.id.clone(),
                    node_id: t.node_id.clone(),
                    state: token_status_str(&t.status).to_string(),
                })
                .collect(),
        ),
        None => (false, vec![]),
    };
    Ok(Json(ReplaySnapshotResponse {
        cursor: session.cursor,
        total_events: session.total_events(),
        completed,
        tokens,
    }))
}

/// DELETE /api/v1/replay/:session_id — destroy session.
pub async fn delete_replay_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut guard = state.replay_sessions.write().unwrap();
    guard.remove(&session_id);
    Ok(StatusCode::NO_CONTENT)
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
            .route("/process-instances/:id/trace", get(get_instance_trace))
            .route("/process-instances/:id/history", get(get_instance_history))
            .route("/process-instances/:id/replay", post(create_replay))
            .route("/replay/:session_id/step", post(replay_step))
            .route("/replay/:session_id/seek", post(replay_seek))
            .route("/replay/:session_id/snapshot", get(get_replay_snapshot))
            .route("/replay/:session_id", delete(delete_replay_session))
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
