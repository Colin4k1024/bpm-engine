# Deploy and verify

This directory contains Docker Compose and schema for running the BPM engine with PostgreSQL. Use it to validate crash recovery and history consistency.

---

## 30-second verification checklist

This checklist verifies that after an engine crash and restart, history and token state stay consistent with no duplicate completion.

1. **Start the database** (optional; for in-memory backend skip to step 2):
   ```bash
   cd deploy && docker compose up -d
   ```
   If using Postgres, run the schema: `psql $DATABASE_URL -f deploy/schema.sql` (or use the init volume as in docker-compose comments).

2. **Start the engine**
   ```bash
   cargo run -p bpm-engine-server-rest
   ```
   (With Postgres: set `DATABASE_URL` and use the engine build that uses the DB backend.)

3. **Start the payment worker**
   ```bash
   cargo run -p bpm-engine-worker-sdk --example payment
   ```

4. **Trigger a process** that hits the payment task, e.g.:
   ```bash
   curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
     -H "Content-Type: application/json" \
     -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}'
   ```
   Note the returned `instance_id`.

5. **Kill the engine** (simulate crash):
   ```bash
   kill -9 <engine_pid>
   ```

6. **Restart the engine** (same command as step 2).

7. **Verify history**:  
   `GET /api/v1/process-instances/:id/history` for the instance from step 4. Check that events are in order and there is no duplicate token completion (e.g. only one `TokenCompleted` per token, one `ExternalTaskCompleted` per task).

---

## Files

- **docker-compose.yml** — Postgres 16 for the engine (optional; in-memory backend needs no DB).
- **schema.sql** — Tables for process instances, tokens, history, external tasks, timers, etc. See [docs/database-schema.md](../docs/database-schema.md).
