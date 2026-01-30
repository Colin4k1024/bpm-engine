# Core Traits & Module Design

> This document defines the **core Rust traits, modules, and crate boundaries** of the BPM engine.
> The goal is to make the runtime **composable, testable, and strictly layered**.

---

## 1. Design Goals

- Pure Rust, no macro magic required
- Clear ownership boundaries
- Async-first, runtime-agnostic
- Infrastructure replaceable (DB / MQ / Timer)

---

## 2. Crate-Level Structure

Recommended workspace layout:

```
bpm-engine/
├── engine-core/        # Domain model & traits (no IO)
├── engine-runtime/     # Execution engine implementation
├── engine-storage/     # Persistence abstractions
├── engine-adapters/    # DB / MQ / Timer adapters
└── examples/
```

---

## 3. engine-core (Pure Domain)

### 3.1 Core Types

```
ProcessDefinition
ProcessInstance
NodeDefinition
Token
Event
```

No async, no database, no side effects.

---

### 3.2 NodeHandler Trait

```
trait NodeHandler {
    fn execute(&self, ctx: NodeContext) -> NodeResult;
}
```

Rules:
- Must be deterministic
- Must be idempotent
- Must not perform blocking IO

---

### 3.3 CompensationHandler

```
trait CompensationHandler {
    fn compensate(&self, ctx: CompensationContext) -> CompensationResult;
}
```

Optional per node.

---

## 4. engine-runtime (Execution)

### 4.1 Engine Trait

```
trait Engine {
    async fn start(&self, process: ProcessDefinition);
    async fn dispatch(&self);
}
```

Responsible for:
- Token claiming
- Event dispatch
- Retry & timers

---

### 4.2 EventHandler

```
trait EventHandler<E: Event> {
    async fn handle(&self, event: E);
}
```

---

## 5. engine-storage (Persistence)

### 5.1 Storage Traits

```
trait TokenStore {
    async fn claim_ready(&self) -> Option<Token>;
    async fn update(&self, token: Token);
}
```

Similar traits:
- ProcessInstanceStore
- EventStore
- TimerStore
- CompensationStore

---

## 6. engine-adapters (Infrastructure)

Contains concrete implementations:

- PostgresTokenStore
- MySqlTokenStore
- KafkaOutbox
- TokioTimer

Adapters depend on traits, never the reverse.

---

## 7. Dependency Direction

```
engine-core
   ↑
engine-runtime
   ↑
engine-storage
   ↑
engine-adapters
```

Dependencies are strictly one-directional.

---

## 8. Testing Strategy

- engine-core: pure unit tests
- runtime: deterministic integration tests
- adapters: contract tests

---

## 9. Extension Points

Designed extension points:

- Custom NodeHandler
- Custom RetryPolicy
- Custom TimerBackend

No fork required.

---

## 10. Relationship to Other Docs

- Execution semantics: `execution-model.md`
- Saga behavior: `saga.md`
- Recovery guarantees: `recovery.md`

---

> **Good architecture lets you stop thinking about it.**
