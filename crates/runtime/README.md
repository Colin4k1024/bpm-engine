# bpm-runtime

Scheduler and token execution: handlers, pump, gateway evaluation, transition.

## Role

- **BpmEngine** — runs the event loop: consume events, dispatch to handlers, emit new events.
- **EngineContext** — holds store references (ProcessInstanceStore, TokenStore, TimerStore, ExternalTaskStore, etc.).
- **EventHandlers** — ProcessStartHandler, TokenArrivedHandler, UserTaskCompletedHandler, ProcessCompletedHandler, etc.
- Scheduler, pump, transition helpers, gateway (EL) evaluation.

Depends on `bpm-storage` traits; no concrete storage implementation.

## Usage

Wire an `EngineContext` with store implementations (e.g. from `bpm-adapter-memory`), then run `BpmEngine::run_async(initial_event, &mut ctx)`. See [crates/server/rest](../../server/rest) for wiring.

## Documentation

See [docs/architecture.md](../../docs/architecture.md) and [docs/execution-model.md](../../docs/execution-model.md).
