//! API server binary (plan v1.0). Run: cargo run --bin api_server --features api
//! Serves REST: POST /processes/start, GET /processes/:id, POST /tasks/complete.

use bpm_engine::api::http::{router, AppState};
use bpm_engine::dsl::ServiceTaskRegistry;
use bpm_engine::engine::{
    ProcessCompletedHandler, ProcessStartHandler, TokenArrivedHandler, UserTaskCompletedHandler,
    BpmEngine,
};
use bpm_engine::model::{Node, NodeType, OutgoingEdge, ProcessDefinition};
use bpm_engine::persistence::{MemoryRepo, ProcessDefStore};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

fn minimal_process() -> ProcessDefinition {
    ProcessDefinition {
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
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    #[cfg(feature = "observability")]
    {
        let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .expect("prometheus recorder");
        bpm_engine::api::http::set_prometheus_handle(handle);
    }

    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(minimal_process());

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(ProcessCompletedHandler),
        Box::new(UserTaskCompletedHandler),
    ]);

    let registry = Arc::new(ServiceTaskRegistry::new());
    let state = Arc::new(AppState {
        engine,
        repo,
        def_store,
        registry: Some(registry),
    });

    let app = router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("API server listening on http://{}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}
