# bpm-storage

Persistence abstractions for the BPM engine. Defines async traits only; no concrete backend.

## Role

- **ProcessInstanceStore** — load/save process instances, list running.
- **ProcessDefinitionStore** — load process definitions by id.
- **TokenStore** — load/save tokens by instance, claim (CAS), update.
- **TimerStore** — get by id, mark fired, insert, list due.
- **ExternalTaskStore** — create, fetch-and-lock, complete, fail, reclaim expired locks.
- Additional traits: ParallelJoinRepo, CompensationRecordRepo, OutboxRepo (event outbox).

Implementations live in adapter crates (e.g. `bpm-adapter-memory`).

## Usage

Depend on this crate to implement a new persistence backend. The runtime and server depend on these traits.

## Documentation

See [docs/database-schema.md](../../docs/database-schema.md) and [docs/architecture.md](../../docs/architecture.md).
