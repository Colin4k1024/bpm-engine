# Python Worker SDK Design

## Overview

This document describes the design of the Python Worker SDK for the BPM Engine.
The SDK provides a pull-based client and worker runtime for external tasks,
mirroring the Rust Worker SDK's functionality.

## Goals

- **Feature parity** with the Rust Worker SDK: fetch-and-lock, complete, fail, retry
- **Pythonic API**: async/await, type hints, dataclasses
- **Minimal dependencies**: `httpx` for HTTP, `pydantic` for data validation
- **Production-ready**: graceful shutdown, exponential backoff, structured logging

## Architecture

```
bpm_engine_sdk/
    __init__.py     - Public API re-exports
    client.py       - EngineClient (HTTP client for Engine REST API)
    worker.py       - Worker (poll loop and task dispatch)
    handler.py      - TaskHandler ABC and TaskContext
    models.py       - ExternalTask, TaskResult dataclasses
```

## API Design

### EngineClient

HTTP client for the Engine external-task REST API.

```python
client = EngineClient("http://127.0.0.1:3000", tenant_id="tenant-1")

# Fetch and lock tasks
tasks = await client.fetch_and_lock(
    worker_id="worker-1",
    task_types=["payment", "notification"],
    max_tasks=10,
    lock_duration_ms=30_000,
)

# Complete a task
await client.complete("task-123", "worker-1", {"status": "paid"})

# Fail a task
await client.fail("task-123", "worker-1", "timeout", retry_after_ms=5000)
```

### TaskHandler

Abstract base class for task handlers. Users implement `task_type` and `handle`.

```python
class PaymentHandler(TaskHandler):
    @property
    def task_type(self) -> str:
        return "payment"

    async def handle(self, task: ExternalTask, ctx: TaskContext) -> TaskResult:
        amount = float(task.variables.get("amount", "0"))
        # process payment ...
        return TaskResult.complete({"status": "paid", "txn_id": "..."})
```

### Worker

Poll-based worker that fetches tasks and dispatches to handlers.

```python
worker = Worker(
    client,
    [PaymentHandler(), NotificationHandler()],
    worker_id="worker-1",
    poll_interval=1.0,
    max_tasks=10,
)

# Run until cancelled
await worker.start()

# Or run until signal
await worker.stop()
```

## Data Models

### ExternalTask

```python
@dataclass
class ExternalTask:
    task_id: str
    task_type: str
    variables: dict[str, str]
    lock_expire_at: str | None
    retries: int
```

### TaskResult

```python
@dataclass
class TaskResult:
    status: str  # "complete" or "fail"
    variables: dict[str, str]
    error: str
    retry_after_ms: int | None

    @classmethod
    def complete(cls, variables=None) -> TaskResult: ...

    @classmethod
    def fail(cls, error, retry_after_ms=None) -> TaskResult: ...
```

## Dependencies

| Package | Version | Purpose |
|---------|---------|---------|
| `httpx` | `>=0.27,<1` | Async HTTP client |
| `pydantic` | `>=2.0,<3` | Data validation (optional, for future use) |

Dev dependencies: `pytest`, `pytest-asyncio`, `respx` (HTTP mocking)

## Error Handling

- `EngineError(status, message)` for API errors
- Handler panics are caught and reported as task failures
- Fetch errors trigger exponential backoff (1s initial, 30s cap, 5 retries)

## Concurrency Model

- Each task is dispatched as an independent `asyncio.Task`
- No shared mutable state between task handlers
- The poll loop runs sequentially (one fetch at a time)
- Graceful shutdown via `worker.stop()` or task cancellation

## Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| `worker_id` | `worker-{uuid[:8]}` | Unique worker identity |
| `max_tasks` | 10 | Max tasks per fetch |
| `lock_duration_ms` | 30000 | Lock duration in ms |
| `poll_interval` | 1.0 | Seconds between polls |
| `fetch_retry_max` | 5 | Max retries on fetch error |
| `fetch_retry_backoff` | 1.0 | Initial backoff in seconds |

## REST API Compatibility

The SDK targets the same REST API as the Rust SDK:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/v1/external-tasks/fetch-and-lock` | POST | Fetch and lock tasks |
| `/api/v1/external-tasks/:id/complete` | POST | Complete a task |
| `/api/v1/external-tasks/:id/fail` | POST | Fail a task |

## Future Enhancements

- **Extend lock**: heartbeat-based lock extension for long-running tasks
- **Task prioritization**: priority-based task selection
- **Metrics**: Prometheus-compatible task metrics
- **Connection pooling**: configurable HTTP connection pool
- **Circuit breaker**: engine health monitoring with circuit breaker pattern
