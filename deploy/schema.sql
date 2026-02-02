-- BPM engine schema for PostgreSQL (see docs/database-schema.md).
-- Run: psql $DATABASE_URL -f deploy/schema.sql
-- Or use docker-compose: init script runs this on first start.

-- Process instances
CREATE TABLE IF NOT EXISTS process_instance (
    id TEXT PRIMARY KEY,
    process_def_id TEXT NOT NULL,
    tenant_id TEXT,
    status TEXT NOT NULL DEFAULT 'Running',
    version INTEGER NOT NULL DEFAULT 0,
    created_at TEXT,
    updated_at TEXT
);

-- Tokens (execution unit, concurrency boundary)
CREATE TABLE IF NOT EXISTS token (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL REFERENCES process_instance(id),
    node_id TEXT NOT NULL,
    state TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'Forward',
    attempt INTEGER NOT NULL DEFAULT 0,
    version INTEGER NOT NULL DEFAULT 0,
    parallel_group_id TEXT,
    created_at TEXT,
    updated_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_token_state_updated ON token(state, updated_at);
CREATE INDEX IF NOT EXISTS idx_token_parallel_group ON token(parallel_group_id);

-- Execution history (for trace / replay)
CREATE TABLE IF NOT EXISTS history_event (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload JSONB,
    occurred_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_history_instance ON history_event(instance_id);
CREATE INDEX IF NOT EXISTS idx_history_occurred ON history_event(instance_id, occurred_at);

-- External tasks (fetch-and-lock, lease)
CREATE TABLE IF NOT EXISTS external_task (
    task_id TEXT PRIMARY KEY,
    token_id TEXT NOT NULL,
    process_instance_id TEXT NOT NULL,
    task_type TEXT NOT NULL,
    state TEXT NOT NULL,
    lock_owner TEXT,
    lock_expire_at TEXT,
    retries INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    variables JSONB DEFAULT '{}',
    created_at TEXT,
    updated_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_external_task_state_type ON external_task(state, task_type);

-- Timers (persistent, scheduler-driven)
CREATE TABLE IF NOT EXISTS timer (
    id TEXT PRIMARY KEY,
    token_id TEXT NOT NULL,
    due_at BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Scheduled',
    created_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_timer_due ON timer(due_at) WHERE status = 'Scheduled';

-- Parallel join tracking
CREATE TABLE IF NOT EXISTS parallel_join (
    parallel_group_id TEXT PRIMARY KEY,
    expected INTEGER NOT NULL,
    joined INTEGER NOT NULL DEFAULT 0
);

-- Compensation records (saga)
CREATE TABLE IF NOT EXISTS compensation_record (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    handler_ref TEXT,
    "order" INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    created_at TEXT
);

-- Event outbox (durable publish)
CREATE TABLE IF NOT EXISTS event_outbox (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    created_at TEXT
);
