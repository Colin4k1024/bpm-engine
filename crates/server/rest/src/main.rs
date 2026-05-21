//! BPM Engine REST server. Run: cargo run -p bpm-server-rest

mod middleware;
mod replay;
mod routes;
mod state;

use crate::routes::router;
use crate::state::AppState;
use bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_engine_core::{Node, NodeType, OutgoingEdge, ProcessDefinition};
use bpm_engine_runtime::{
    BpmEngine, HistoryHandler, ProcessCompletedHandler, ProcessStartHandler, TokenArrivedHandler,
    UserTaskCompletedHandler,
};
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

/// Process with ExternalTask (task_type = "payment") for worker-sdk payment example.
fn payment_process() -> ProcessDefinition {
    ProcessDefinition {
        id: "payment-flow",
        start: "start",
        nodes: HashMap::from([
            (
                "start",
                Node {
                    id: "start",
                    node_type: NodeType::Start,
                    outgoing_edges: vec![OutgoingEdge {
                        target: "payment",
                        condition: None,
                    }],
                },
            ),
            (
                "payment",
                Node {
                    id: "payment",
                    node_type: NodeType::ExternalTask {
                        task_type: "payment".to_string(),
                        retries: 3,
                        timeout_secs: 60,
                    },
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
    def_store.register(payment_process());

    let engine = BpmEngine::new(vec![
        Box::new(ProcessStartHandler),
        Box::new(TokenArrivedHandler::new()),
        Box::new(ProcessCompletedHandler),
        Box::new(UserTaskCompletedHandler),
        Box::new(HistoryHandler),
    ]);

    let state = Arc::new(AppState {
        engine,
        repo,
        def_store,
        replay_sessions: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
    });

    let app = router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("BPM Engine REST server listening on http://{}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}
