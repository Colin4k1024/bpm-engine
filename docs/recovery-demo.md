# Recovery demo: kill → restart → state from DB

This document describes how to demonstrate **crash-safe execution**: run the engine, start a process, run a few steps, kill the process, restart, and show that state is recovered from the persistence layer (or that replay reproduces the same state).

## Prerequisites

- Engine REST server (e.g. `cargo run -p bpm-engine-server-rest`) with a persistence backend.
- Default in-memory backend: state is lost on restart; replay can still be used if history was written (e.g. via REST with HistoryRepo).
- For "golden path" with Postgres: use `deploy/docker-compose.yml` and `deploy/schema.sql`, then run the engine configured with `DATABASE_URL` (when a Postgres adapter is available).

## Steps (in-memory + history)

1. **Start the engine**
   ```bash
   cargo run -p bpm-engine-server-rest
   ```

2. **Deploy a process and start an instance**
   ```bash
   # Deploy (if BPMN deploy is used) or use built-in payment-flow
   curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
     -H "Content-Type: application/json" \
     -d '{"process_def_id":"payment-flow","variables":{}}'
   # Note the instance_id from the response.
   ```

3. **Run a few steps** (e.g. worker fetches and completes the payment task).

4. **Inspect state and history**
   ```bash
   curl http://127.0.0.1:3000/api/v1/process-instances/<instance_id>
   curl http://127.0.0.1:3000/api/v1/process-instances/<instance_id>/trace
   curl http://127.0.0.1:3000/api/v1/process-instances/<instance_id>/history
   ```

5. **Kill the server** (e.g. `kill -9 <pid>`).

6. **Restart the server**
   ```bash
   cargo run -p bpm-engine-server-rest
   ```

7. **With in-memory backend**: Instance state is gone; if you had written history via the REST server, you can use **replay** to reconstruct state:
   ```bash
   curl -X POST http://127.0.0.1:3000/api/v1/process-instances/<instance_id>/replay
   # Use session_id from response
   curl http://127.0.0.1:3000/api/v1/replay/<session_id>/snapshot
   # Step through: POST .../replay/<session_id>/step
   ```

8. **With Postgres backend** (when implemented): After restart, the engine re-reads instances, tokens, and history from the database. `GET /process-instances/<id>` and `GET /process-instances/<id>/trace` return the same state as before the kill, proving recovery from DB.

## Verifying recovery

- **Trace API**: Before and after restart (with persistent storage), `GET /api/v1/process-instances/:id/trace` should return the same `instance`, `token_timelines`, and `external_task_history` for the same instance.
- **Replay**: Replay the instance’s history; the final snapshot should match the last persisted state (same tokens and status).

## Postgres golden path

1. Start Postgres: `cd deploy && docker compose up -d`
2. Schema is applied automatically from `deploy/schema.sql` on first start (via init script).
3. When the engine has a Postgres adapter, set `DATABASE_URL=postgres://postgres:postgres@localhost:5432/bpm` and run the engine; state survives `kill -9` and restart.

See [database-schema.md](database-schema.md) and [recovery.md](recovery.md) for design details.
