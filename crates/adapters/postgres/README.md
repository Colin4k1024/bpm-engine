# bpm-engine-adapter-postgres

PostgreSQL persistence adapter for the BPM engine. Implements all10 storage traits
defined in `bpm-engine-storage` using `tokio-postgres` and `deadpool-postgres`.

## Quick Start

```rust
use bpm_engine_adapter_postgres::{create_pool, migrate, PostgresTokenStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pool = create_pool("postgres://postgres:postgres@localhost:5432/bpm")?;
    migrate(&pool).await?;

    let token_store = PostgresTokenStore::new(pool);
    // Use with BpmEngine...
    Ok(())
}
```

## Connection Configuration

The `create_pool` function accepts a standard PostgreSQL connection URL:

```
postgres://USER:PASSWORD@HOST:PORT/DATABASE
```

| Parameter | Default | Description |
|-----------|---------|-------------|
| `USER` | — | PostgreSQL username |
| `PASSWORD` | — | PostgreSQL password |
| `HOST` | `localhost` | Database host |
| `PORT` | `5432` | Database port |
| `DATABASE` | — | Database name |

### Environment Variable

```bash
export DATABASE_URL="postgres://postgres:postgres@localhost:5432/bpm"
```

```rust
let url = std::env::var("DATABASE_URL")?;
let pool = create_pool(&url)?;
```

### Connection Pool

The adapter uses `deadpool-postgres` with default pool settings. Pool size and
timeouts can be configured via the connection URL or environment variables:

```bash
export DEADPOOL_MAX_SIZE=32
export DEADPOOL_TIMEOUTS_WAIT_SECS=5
export DEADPOOL_TIMEOUTS_CREATE_SECS=5
export DEADPOOL_TIMEOUTS_RECYCLE_SECS=60
```

## Schema Migration

Run `migrate()` once at application startup. It creates all required tables
(idempotent — safe to call multiple times):

```rust
migrate(&pool).await?;
```

Tables created:
- `process_definition` — BPMN definitions with versioning
- `process_instance` — Runtime process instances
- `token` — Execution tokens with CAS-based updates
- `external_task` — Worker tasks with lease-based locking
- `timer` — Scheduled timers for boundary and intermediate events
- `compensation_record` — Saga compensation tracking
- `outbox_event` — Event outbox for reliable messaging
- `history_event` — Append-only audit trail
- `dead_letter_entry` — Failed tasks after retry exhaustion
- `parallel_join` — Parallel gateway join tracking

## Implemented Stores

| Store | Struct | Description |
|-------|--------|-------------|
| `TokenStore` | `PostgresTokenStore` | Token CRUD with CAS updates |
| `ProcessInstanceStore` | `PostgresProcessStore` | Instance lifecycle |
| `ProcessDefinitionStore` | `PostgresProcessDefStore` | Definition deploy/load/version |
| `ExternalTaskStore` | `PostgresExternalTaskStore` | Task create/lock/complete/fail/reclaim |
| `TimerStore` | `PostgresTimerStore` | Timer insert/list_due/mark_fired |
| `CompensationRecordRepo` | `PostgresCompensationRepo` | Compensation record tracking |
| `OutboxRepo` | `PostgresOutboxRepo` | Event outbox |
| `HistoryRepo` | `PostgresHistoryRepo` | Append-only history |
| `DeadLetterStore` | `PostgresDeadLetterStore` | Dead letter queue |
| `ParallelJoinRepo` | `PostgresParallelJoinRepo` | Join counter management |

## Running Tests

Integration tests require Docker (uses testcontainers):

```bash
# Run all Postgres integration tests
cargo test -p bpm-engine-adapter-postgres -- --ignored

# Run a specific test
cargo test -p bpm-engine-adapter-postgres -- --ignored token_store_save_and_load
```

## Docker Compose

```yaml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: postgres
      POSTGRES_DB: bpm
    ports:
      - "5432:5432"
    volumes:
      - pgdata:/var/lib/postgresql/data
      - ./deploy/schema.sql:/docker-entrypoint-initdb.d/01-schema.sql

volumes:
  pgdata:
```
