# bpm-adapter-memory

In-memory implementation of the storage traits for the BPM engine.

## Role

- Implements **ProcessInstanceStore**, **ProcessDefinitionStore**, **TokenStore**, **TimerStore**, **ExternalTaskStore**, plus ParallelJoinRepo, CompensationRecordRepo, OutboxRepo, EventStore.
- **MemoryRepo** — single struct implementing multiple store traits for development and single-node testing.
- **ProcessDefStore** — in-memory process definition store.

No database required. State is lost on restart.

## Usage

Use for quick start, examples, and tests. For production persistence, use or implement a backend (e.g. Postgres, SQLite) that implements the same traits from `bpm-storage`.

## Documentation

See [docs/database-schema.md](../../../docs/database-schema.md) and [docs/architecture.md](../../../docs/architecture.md).
