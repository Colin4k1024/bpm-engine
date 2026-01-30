//! REST API (plan v1.0): POST /processes/start, GET /processes/:id, POST /tasks/complete.

#[cfg(feature = "api")]
mod server {
    use crate::api::service::{ProcessService, TaskService};
    use crate::engine::{BpmEngine, EngineContext};
    use crate::model::ProcessInstance;
    use crate::persistence::{MemoryRepo, ProcessDefStore, ProcessDefinitionRepo};
    use axum::{
        extract::{Path, State},
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::sync::Arc;

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
    }

    #[derive(Deserialize)]
    pub struct StartProcessRequest {
        pub process_id: String,
        pub instance_id: Option<String>,
    }

    #[derive(Serialize)]
    pub struct StartProcessResponse {
        pub instance_id: String,
    }

    #[derive(Deserialize)]
    pub struct CompleteTaskRequest {
        pub instance_id: String,
        pub node_id: String,
        pub task_id: String,
        pub variables: Option<HashMap<String, String>>,
    }

    #[derive(Serialize)]
    pub struct ErrorResponse {
        pub error: String,
    }

    fn build_ctx(state: &AppState) -> EngineContext {
        let repo = Arc::clone(&state.repo);
        EngineContext {
            process_repo: Some(Box::new(Arc::clone(&repo))),
            token_repo: Some(Box::new(Arc::clone(&repo))),
            process_def_repo: Some(Box::new(Arc::clone(&state.def_store))),
            task_repo: None,
            parallel_join_repo: Some(Box::new(Arc::clone(&repo))),
            timer_repo: Some(Box::new(Arc::clone(&repo))),
            compensation_repo: Some(Box::new(Arc::clone(&repo))),
            run_in_tx: Some(Box::new(move |event, handlers, ctx, queue| {
                for handler in handlers {
                    let new_events = handler.handle(event, ctx);
                    queue.extend(new_events);
                }
            })),
        }
    }

    pub async fn start_process(
        State(state): State<Arc<AppState>>,
        Json(body): Json<StartProcessRequest>,
    ) -> Result<Json<StartProcessResponse>, (StatusCode, Json<ErrorResponse>)> {
        let mut ctx = build_ctx(state.as_ref());
        match ProcessService::start_process(
            &body.process_id,
            body.instance_id.clone(),
            &state.engine,
            &mut ctx,
        ) {
            Ok(instance_id) => Ok(Json(StartProcessResponse { instance_id })),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )),
        }
    }

    pub async fn get_process(
        State(state): State<Arc<AppState>>,
        Path(id): Path<String>,
    ) -> Result<Json<ProcessInstance>, (StatusCode, Json<ErrorResponse>)> {
        let ctx = build_ctx(state.as_ref());
        match ProcessService::get_process(&id, &ctx) {
            Some(inst) => Ok(Json(inst)),
            None => Err((
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("process instance not found: {}", id),
                }),
            )),
        }
    }

    pub async fn get_definition_diagram(
        State(state): State<Arc<AppState>>,
        Path(id): Path<String>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        match state.def_store.load(&id) {
            Some(def) => {
                let body = definition_to_mermaid(&def);
                ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
            }
            None => (StatusCode::NOT_FOUND, Json(ErrorResponse { error: format!("definition not found: {}", id) })).into_response(),
        }
    }

    pub async fn complete_task(
        State(state): State<Arc<AppState>>,
        Json(body): Json<CompleteTaskRequest>,
    ) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
        let variables = body.variables.unwrap_or_default();
        let mut ctx = build_ctx(state.as_ref());
        match TaskService::complete_task(
            &body.instance_id,
            &body.node_id,
            &body.task_id,
            variables,
            &state.engine,
            &mut ctx,
        ) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )),
        }
    }

    #[cfg(feature = "observability")]
    static PROMETHEUS_HANDLE: std::sync::OnceLock<metrics_exporter_prometheus::PrometheusHandle> =
        std::sync::OnceLock::new();

    /// Call once at startup when observability feature is enabled to expose GET /metrics.
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
                ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
            }
            None => (axum::http::StatusCode::SERVICE_UNAVAILABLE, "metrics not initialized").into_response(),
        }
    }

    pub fn router(state: Arc<AppState>) -> Router {
        let r = Router::new()
            .route("/processes/start", post(start_process))
            .route("/processes/:id", get(get_process))
            .route("/definitions/:id/diagram", get(get_definition_diagram))
            .route("/tasks/complete", post(complete_task))
            .with_state(state);
        #[cfg(feature = "observability")]
        let r = r.route("/metrics", get(metrics_handler));
        r
    }
}

#[cfg(feature = "api")]
pub use server::{router, AppState, CompleteTaskRequest, ErrorResponse, StartProcessRequest, StartProcessResponse};
#[cfg(all(feature = "api", feature = "observability"))]
pub use server::set_prometheus_handle;
