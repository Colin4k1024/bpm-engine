//! BPM Engine REST server. Run: cargo run -p bpm-server-rest

mod routes;
mod state;

use bpm_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_core::{Node, NodeType, OutgoingEdge, ProcessDefinition};
use bpm_runtime::{
    BpmEngine, ProcessCompletedHandler, ProcessStartHandler, TokenArrivedHandler,
    UserTaskCompletedHandler,
};
use crate::routes::router;
use crate::state::AppState;
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

    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());
    def_store.register(minimal_process());

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(ProcessCompletedHandler),
        Box::new(UserTaskCompletedHandler),
    ]);

    let state = Arc::new(AppState {
        engine,
        repo,
        def_store,
    });

    let app = router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("BPM Engine REST server listening on http://{}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}
