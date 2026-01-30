//! REST API v1 (docs_api_spec): /api/v1 process-definitions, process-instances, tasks, signals, tokens.

#[cfg(feature = "api")]
mod server {
    use crate::api::service::{ProcessService, TaskService, TaskListItem};
    use crate::engine::{BpmEngine, EngineContext};
    use crate::model::{InstanceState, ProcessInstance};
    use crate::persistence::{MemoryRepo, ProcessDefStore, ProcessDefinitionRepo};
    use axum::{
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Optional registry for DSL deploy. When present, POST /process-definitions can deploy JSON definition.
    pub type ServiceTaskRegistry = crate::dsl::ServiceTaskRegistry;

    /// Render process definition as Mermaid flowchart (plan v2.0 E.1).
    fn definition_to_mermaid(def: &crate::model::ProcessDefinition) -> String {
        let mut lines = vec!["flowchart LR".to_string()];
        for (_node_id, node) in &def.nodes {
            let from = escape_mermaid_id(node.id);
            for edge in &node.outgoing_edges {
                let to = escape_mermaid_id(edge.target);
                lines.push(format!("{} --> {}", from, to));
            }
        }
        lines.join("\n")
    }

    fn escape_mermaid_id(id: &str) -> String {
        if id.chars().any(|c| !c.is_ascii_alphanumeric() && c != '_') {
            format!("\"{}\"", id.replace('"', "\\\""))
        } else {
            id.to_string()
        }
    }

    /// Shared app state. Pass Arc<AppState> to router (Arc is Clone + Send + Sync).
    pub struct AppState {
        pub engine: BpmEngine,
        pub repo: Arc<MemoryRepo>,
        pub def_store: Arc<ProcessDefStore>,
        /// When Some, POST /process-definitions can deploy DSL JSON.
        pub registry: Option<Arc<ServiceTaskRegistry>>,
    }

    // ---------- DTOs (spec-aligned) ----------

    #[derive(Deserialize)]
    pub struct DeployRequest {
        pub id: String,
        pub version: String,
        pub definition: serde_json::Value,
    }

    #[derive(Serialize)]
    pub struct DeployResponse {
        pub id: String,
        pub version: String,
        pub status: String,
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

    #[derive(Deserialize)]
    #[allow(dead_code)] // signal, payload reserved for future boundary/message events
    pub struct SignalRequest {
        pub signal: String,
        pub instance_id: String,
        #[serde(default)]
        pub payload: HashMap<String, serde_json::Value>,
    }

    #[derive(Serialize)]
    pub struct ErrorResponse {
        pub error: String,
    }

    /// v3: tenant_id from X-Tenant-Id header (optional).
    fn build_ctx(state: &AppState, tenant_id: Option<String>) -> EngineContext {
        let repo = Arc::clone(&state.repo);
        EngineContext {
            process_repo: Some(Box::new(Arc::clone(&repo))),
            token_repo: Some(Box::new(Arc::clone(&repo))),
            process_def_repo: Some(Box::new(Arc::clone(&state.def_store))),
            task_repo: None,
            parallel_join_repo: Some(Box::new(Arc::clone(&repo))),
            timer_repo: Some(Box::new(Arc::clone(&repo))),
            compensation_repo: Some(Box::new(Arc::clone(&repo))),
            outbox_repo: None,
            tenant_id,
            run_in_tx: Some(Box::new(move |event, handlers, ctx, queue| {
                for handler in handlers {
                    let new_events = handler.handle(event, ctx);
                    queue.extend(new_events);
                }
            })),
        }
    }

    fn tenant_from_headers(headers: &HeaderMap) -> Option<String> {
        headers
            .get("x-tenant-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    }

    /// Idempotency-Key (API spec §5): reserved for POST process-instances and POST tasks/:id/complete.
    /// When implemented, same key + same body returns cached response.
    #[allow(dead_code)]
    fn idempotency_key_from_headers(headers: &HeaderMap) -> Option<String> {
        headers
            .get("idempotency-key")
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
        use crate::model::TokenStatus;
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

    // ---------- Handlers ----------

    /// POST /api/v1/process-definitions — deploy a process definition (DSL JSON).
    pub async fn deploy_process(
        State(state): State<Arc<AppState>>,
        Json(body): Json<DeployRequest>,
    ) -> Result<(StatusCode, Json<DeployResponse>), (StatusCode, Json<ErrorResponse>)> {
        let registry = state
            .registry
            .as_ref()
            .ok_or((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "DSL deploy not configured (no registry)".to_string(),
                }),
            ))?;
        let def_json = serde_json::to_string(&body.definition).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid definition JSON: {}", e),
                }),
            )
        })?;
        let dsl = crate::dsl::load_from_json(&def_json)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("definition parse error: {}", e),
                }),
            )
        })?;
        let def = crate::dsl::to_process_definition(&dsl, registry.as_ref()).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
        if def.id != body.id {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("definition id mismatch: expected {}", body.id),
                }),
            ));
        }
        state.def_store.register(def);
        Ok((
            StatusCode::CREATED,
            Json(DeployResponse {
                id: body.id,
                version: body.version,
                status: "DEPLOYED".to_string(),
            }),
        ))
    }

    /// POST /api/v1/process-instances — start a process instance.
    /// Idempotency-Key header accepted (cached response TBD).
    pub async fn start_instance(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Json(body): Json<StartInstanceRequest>,
    ) -> Result<(StatusCode, Json<StartInstanceResponse>), (StatusCode, Json<ErrorResponse>)> {
        let _key = idempotency_key_from_headers(&headers);
        let tenant_id = tenant_from_headers(&headers);
        let mut ctx = build_ctx(state.as_ref(), tenant_id);
        match ProcessService::start_process(
            &body.process_def_id,
            None,
            body.variables,
            &state.engine,
            &mut ctx,
        ) {
            Ok(instance_id) => Ok((
                StatusCode::CREATED,
                Json(StartInstanceResponse {
                    instance_id: instance_id.clone(),
                    status: "RUNNING".to_string(),
                }),
            )),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )),
        }
    }

    /// GET /api/v1/process-instances/:id — get instance state (spec: instance_id, status, current_nodes).
    pub async fn get_instance(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(id): Path<String>,
    ) -> Result<Json<InstanceStateResponse>, (StatusCode, Json<ErrorResponse>)> {
        let tenant_id = tenant_from_headers(&headers);
        let ctx = build_ctx(state.as_ref(), tenant_id);
        match ProcessService::get_process(&id, &ctx) {
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

    /// POST /api/v1/process-instances/:id/cancel — cancel (terminate) instance.
    pub async fn cancel_instance(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(id): Path<String>,
    ) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
        let tenant_id = tenant_from_headers(&headers);
        let mut ctx = build_ctx(state.as_ref(), tenant_id);
        match ProcessService::cancel_instance(&id, &mut ctx) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(e) => {
                let msg = e.to_string();
                let code = if msg.contains("not found") {
                    StatusCode::NOT_FOUND
                } else if msg.contains("not running") {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                Err((
                    code,
                    Json(ErrorResponse { error: msg }),
                ))
            }
        }
    }

    /// GET /api/v1/tasks?type=user|external — list pending tasks.
    pub async fn list_tasks(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Query(params): Query<HashMap<String, String>>,
    ) -> Result<Json<Vec<TaskListItem>>, (StatusCode, Json<ErrorResponse>)> {
        let tenant_id = tenant_from_headers(&headers);
        let ctx = build_ctx(state.as_ref(), tenant_id);
        let type_filter = params.get("type").map(String::as_str);
        let list = TaskService::list_tasks(&ctx, type_filter);
        Ok(Json(list))
    }

    /// Parse task_id as "instance_id:node_id" (instance_id may contain colons, e.g. UUID).
    fn parse_task_id(task_id: &str) -> Option<(String, String)> {
        let mut parts = task_id.rsplitn(2, ':');
        let node_id = parts.next()?.to_string();
        let instance_id = parts.next()?.to_string();
        if instance_id.is_empty() || node_id.is_empty() {
            return None;
        }
        Some((instance_id, node_id))
    }

    /// POST /api/v1/tasks/:task_id/complete — complete a task by task_id.
    /// Idempotency-Key header accepted (cached response TBD).
    pub async fn complete_task_by_id(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(task_id): Path<String>,
        Json(body): Json<CompleteTaskBodyRequest>,
    ) -> Result<Json<CompleteTaskResponse>, (StatusCode, Json<ErrorResponse>)> {
        let _key = idempotency_key_from_headers(&headers);
        let (instance_id, node_id) = parse_task_id(&task_id).ok_or((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid task_id: {}", task_id),
            }),
        ))?;
        let tenant_id = tenant_from_headers(&headers);
        let mut ctx = build_ctx(state.as_ref(), tenant_id);
        match TaskService::complete_task(
            &instance_id,
            &node_id,
            &task_id,
            body.variables,
            &state.engine,
            &mut ctx,
        ) {
            Ok(()) => Ok(Json(CompleteTaskResponse {
                status: "COMPLETED".to_string(),
            })),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )),
        }
    }

    /// POST /api/v1/signals — send signal (minimal: validate instance exists and Running, return 202).
    pub async fn send_signal(
        State(state): State<Arc<AppState>>,
        Json(body): Json<SignalRequest>,
    ) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
        let tenant_id = None::<String>;
        let ctx = build_ctx(state.as_ref(), tenant_id);
        let inst = ProcessService::get_process(&body.instance_id, &ctx);
        if inst.map(|i| i.state == InstanceState::Running).unwrap_or(false) {
            Ok(StatusCode::ACCEPTED)
        } else {
            Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("instance not found or not running: {}", body.instance_id),
                }),
            ))
        }
    }

    /// POST /api/v1/tokens/:token_id/retry — retry a token.
    pub async fn retry_token(
        State(state): State<Arc<AppState>>,
        headers: HeaderMap,
        Path(token_id): Path<String>,
    ) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
        let tenant_id = tenant_from_headers(&headers);
        let mut ctx = build_ctx(state.as_ref(), tenant_id);
        match ProcessService::retry_token(&token_id, &state.engine, &mut ctx) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(e) => {
                let msg = e.to_string();
                let code = if msg.contains("not found") || msg.contains("not retriable") {
                    StatusCode::NOT_FOUND
                } else if msg.contains("conflict") {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                Err((
                    code,
                    Json(ErrorResponse { error: msg }),
                ))
            }
        }
    }

    /// GET /api/v1/process-definitions/:id/diagram — Mermaid diagram.
    pub async fn get_definition_diagram(
        State(state): State<Arc<AppState>>,
        Path(id): Path<String>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        match state.def_store.load(&id) {
            Some(def) => {
                let body = definition_to_mermaid(&def);
                ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
                    .into_response()
            }
            None => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("definition not found: {}", id),
                }),
            )
                .into_response(),
        }
    }

    #[cfg(feature = "observability")]
    static PROMETHEUS_HANDLE: std::sync::OnceLock<metrics_exporter_prometheus::PrometheusHandle> =
        std::sync::OnceLock::new();

    #[cfg(feature = "observability")]
    pub fn set_prometheus_handle(handle: metrics_exporter_prometheus::PrometheusHandle) {
        let _ = PROMETHEUS_HANDLE.set(handle);
    }

    #[cfg(feature = "observability")]
    async fn metrics_handler() -> axum::response::Response {
        use axum::response::IntoResponse;
        match PROMETHEUS_HANDLE.get() {
            Some(handle) => {
                let body = handle.render();
                ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
                    .into_response()
            }
            None => (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "metrics not initialized",
            )
                .into_response(),
        }
    }

    pub fn router(state: Arc<AppState>) -> Router {
        let v1 = Router::new()
            .route("/process-definitions", post(deploy_process))
            .route(
                "/process-definitions/:id/diagram",
                get(get_definition_diagram),
            )
            .route("/process-instances", post(start_instance))
            .route("/process-instances/:id", get(get_instance))
            .route("/process-instances/:id/cancel", post(cancel_instance))
            .route("/tasks", get(list_tasks))
            .route("/tasks/:task_id/complete", post(complete_task_by_id))
            .route("/signals", post(send_signal))
            .route("/tokens/:token_id/retry", post(retry_token))
            .with_state(state.clone());

        let r = Router::new()
            .nest("/api/v1", v1);
        #[cfg(feature = "observability")]
        let r = r.route("/metrics", get(metrics_handler));
        r
    }
}

#[cfg(feature = "api")]
pub use server::{
    router, AppState, DeployRequest, DeployResponse, ErrorResponse, StartInstanceRequest,
    StartInstanceResponse,
};
#[cfg(all(feature = "api", feature = "observability"))]
pub use server::set_prometheus_handle;
