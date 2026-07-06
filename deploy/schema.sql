-- BPM engine schema for PostgreSQL.
-- This file is the canonical schema reference, aligned with migrate() in
-- crates/adapters/postgres/src/lib.rs.
--
-- Run: psql $DATABASE_URL -f deploy/schema.sql

-- Process definitions (BPMN XML stored for runtime compilation)
CREATE TABLE IF NOT EXISTS process_definition (
    id TEXT PRIMARY KEY,
    def_key TEXT NOT NULL DEFAULT '',
    version INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'active',
    bpmn_xml TEXT NOT NULL,
    created_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_process_def_key_version ON process_definition (def_key, version);

-- Process instances
CREATE TABLE IF NOT EXISTS process_instance (
    id TEXT PRIMARY KEY,
    process_def_id TEXT NOT NULL,
    tenant_id TEXT,
    variables TEXT NOT NULL DEFAULT '{}',
    state TEXT NOT NULL DEFAULT 'Running',
    version INTEGER NOT NULL DEFAULT 1,
    parent_instance_id TEXT,
    parent_token_id TEXT,
    created_at TEXT,
    updated_at TEXT
);

-- Tokens (execution unit, concurrency boundary)
CREATE TABLE IF NOT EXISTS token (
    id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Created',
    mode TEXT NOT NULL DEFAULT 'Forward',
    version INTEGER NOT NULL DEFAULT 1,
    attempt INTEGER NOT NULL DEFAULT 0,
    parallel_group_id TEXT,
    created_at TEXT,
    updated_at TEXT,
    PRIMARY KEY (id, instance_id),
    CONSTRAINT fk_token_instance FOREIGN KEY (instance_id)
        REFERENCES process_instance(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_token_state_updated ON token(status, updated_at);
CREATE INDEX IF NOT EXISTS idx_token_parallel_group ON token(parallel_group_id);

-- External tasks (fetch-and-lock, lease)
CREATE TABLE IF NOT EXISTS external_task (
    id TEXT PRIMARY KEY,
    token_id TEXT NOT NULL,
    process_instance_id TEXT NOT NULL,
    task_type TEXT NOT NULL,
    retries INTEGER NOT NULL DEFAULT 3,
    timeout_secs INTEGER NOT NULL DEFAULT 300,
    variables TEXT NOT NULL DEFAULT '{}',
    state TEXT NOT NULL DEFAULT 'Ready',
    worker_id TEXT,
    lock_expire_at TEXT,
    error_message TEXT,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT,
    updated_at TEXT,
    CONSTRAINT fk_external_task_instance FOREIGN KEY (process_instance_id)
        REFERENCES process_instance(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_external_task_state ON external_task(state, lock_expire_at);
CREATE INDEX IF NOT EXISTS idx_external_task_type ON external_task(task_type);

-- Timers (persistent, scheduler-driven)
CREATE TABLE IF NOT EXISTS timer (
    id TEXT PRIMARY KEY,
    token_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    node_id TEXT NOT NULL DEFAULT '',
    due_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Scheduled',
    created_at TEXT NOT NULL,
    CONSTRAINT fk_timer_instance FOREIGN KEY (instance_id)
        REFERENCES process_instance(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_timer_due ON timer(status, due_at);

-- Execution history (for trace / replay)
CREATE TABLE IF NOT EXISTS history_event (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    CONSTRAINT fk_history_instance FOREIGN KEY (instance_id)
        REFERENCES process_instance(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_history_instance ON history_event(instance_id, occurred_at);

-- Event outbox (durable publish)
CREATE TABLE IF NOT EXISTS event_outbox (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    created_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_outbox_status ON event_outbox(status, created_at);

-- Compensation records (saga)
CREATE TABLE IF NOT EXISTS compensation_record (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    handler_ref TEXT,
    "order" INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    created_at TEXT,
    CONSTRAINT fk_compensation_instance FOREIGN KEY (instance_id)
        REFERENCES process_instance(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_compensation_instance ON compensation_record(instance_id, "order");

-- Parallel join tracking
CREATE TABLE IF NOT EXISTS parallel_join (
    parallel_group_id TEXT PRIMARY KEY,
    expected INTEGER NOT NULL,
    joined INTEGER NOT NULL DEFAULT 0
);

-- Dead letter queue (failed external tasks after retries exhausted)
CREATE TABLE IF NOT EXISTS dead_letter (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    process_instance_id TEXT NOT NULL,
    task_type TEXT NOT NULL,
    error_message TEXT NOT NULL DEFAULT '',
    variables TEXT NOT NULL DEFAULT '{}',
    tenant_id TEXT,
    created_at TEXT NOT NULL,
    CONSTRAINT fk_dead_letter_instance FOREIGN KEY (process_instance_id)
        REFERENCES process_instance(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_dead_letter_instance ON dead_letter(process_instance_id);
CREATE INDEX IF NOT EXISTS idx_dead_letter_type ON dead_letter(task_type);
