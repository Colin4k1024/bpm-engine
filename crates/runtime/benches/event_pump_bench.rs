//! Benchmarks for EventPump throughput (#26).
//!
//! Measures:
//! - Event dispatch throughput (events/second) with a no-op handler
//! - Impact of handler count on dispatch latency

use async_trait::async_trait;
use bpm_engine_adapter_memory::{MemoryRepo, ProcessDefStore};
use bpm_engine_core::{EngineEvent, TokenStatus};
use bpm_engine_runtime::{EngineContext, EventHandler, EventPump};
use bpm_engine_storage::ProcessInstanceStore;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// No-op handler: consumes events without producing new ones
// ---------------------------------------------------------------------------

struct NoopHandler;

#[async_trait]
impl EventHandler for NoopHandler {
    async fn handle(&self, _event: &EngineEvent, _ctx: &mut EngineContext) -> Vec<EngineEvent> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Counting handler: counts invocations, produces no follow-up events
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct CountingHandler {
    count: std::sync::atomic::AtomicU64,
}

#[allow(dead_code)]
impl CountingHandler {
    fn new() -> Self {
        Self {
            count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn count(&self) -> u64 {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait]
impl EventHandler for CountingHandler {
    async fn handle(&self, _event: &EngineEvent, _ctx: &mut EngineContext) -> Vec<EngineEvent> {
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_token_arrived_event() -> EngineEvent {
    EngineEvent::TokenArrived(bpm_engine_core::event::payloads::TokenArrived {
        instance_id: "bench-instance".to_string(),
        token_id: "token-1".to_string(),
        node_id: "start".to_string(),
    })
}

async fn make_ctx() -> EngineContext {
    let repo = Arc::new(MemoryRepo::new());
    let def_store = Arc::new(ProcessDefStore::new());

    // Create a minimal process instance for the benchmark
    use bpm_engine_core::{InstanceState, ProcessInstance, Token, TokenMode};
    let instance = ProcessInstance {
        id: "bench-instance".to_string(),
        process_def_id: "bench-process".to_string(),
        tenant_id: None,
        tokens: vec![Token {
            id: "token-1".to_string(),
            node_id: "start".to_string(),
            status: TokenStatus::Ready,
            mode: TokenMode::Forward,
            version: 1,
            attempt: 0,
            parallel_group_id: None,
            updated_at: None,
        }],
        variables: std::collections::HashMap::new(),
        state: InstanceState::Running,
        version: 1,
        parent_instance_id: None,
        parent_token_id: None,
    };
    repo.save(&instance).await.unwrap();

    EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn bpm_engine_storage::TokenStore>,
        def_store as Arc<dyn bpm_engine_storage::ProcessDefinitionStore>,
    )
    .build()
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_event_pump_noop(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("event_pump/noop_handler/single_event", |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                rt.block_on(async {
                    let mut ctx = make_ctx().await;
                    let handlers: Vec<Box<dyn EventHandler>> = vec![Box::new(NoopHandler)];
                    let event = make_token_arrived_event();
                    EventPump::run_async(&handlers, black_box(event), &mut ctx).await;
                });
            }
            start.elapsed()
        });
    });
}

fn bench_event_pump_handler_scaling(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    for num_handlers in [1, 5, 10, 25] {
        let bench_name = format!("event_pump/{}_handlers", num_handlers);
        c.bench_function(&bench_name, |b| {
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    rt.block_on(async {
                        let mut ctx = make_ctx().await;
                        let handlers: Vec<Box<dyn EventHandler>> = (0..num_handlers)
                            .map(|_| Box::new(NoopHandler) as Box<dyn EventHandler>)
                            .collect();
                        let event = make_token_arrived_event();
                        EventPump::run_async(&handlers, black_box(event), &mut ctx).await;
                    });
                }
                start.elapsed()
            });
        });
    }
}

fn bench_event_pump_batch(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("event_pump/batch_100_events", |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                rt.block_on(async {
                    let mut ctx = make_ctx().await;
                    let handlers: Vec<Box<dyn EventHandler>> = vec![Box::new(NoopHandler)];
                    // Process 100 independent events sequentially
                    for _ in 0..100 {
                        let event = make_token_arrived_event();
                        EventPump::run_async(&handlers, event, &mut ctx).await;
                    }
                });
            }
            start.elapsed()
        });
    });
}

criterion_group!(
    benches,
    bench_event_pump_noop,
    bench_event_pump_handler_scaling,
    bench_event_pump_batch,
);
criterion_main!(benches);
