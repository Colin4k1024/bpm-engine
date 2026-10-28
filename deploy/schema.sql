-- BPM engine schema for PostgreSQL.
--
-- REFERENCE ARTIFACT (generated, kept in sync with the migration scripts).
-- The source of truth for schema changes is
-- crates/adapters/postgres/migrations/ (versioned, forward-only SQL files
-- applied by the engine's migration runner at startup; see ADR-005 and
-- docs/database-schema.md). Do not hand-edit this file to change the schema —
-- add a new migration under migrations/ instead, then regenerate/refresh this
-- reference. This file reflects migrations 0001 + 0002 + 0003 + 0004. The
-- migration runner additionally maintains a `schema_migrations` bookkeeping
-- table (version, name, applied_at) — a database initialized from this file is
-- recognized by the runner as a pre-versioning schema and baseline-stamped.
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
-- Added in migration 0002 (hot-path query index: list_running scans)
CREATE INDEX IF NOT EXISTS idx_process_instance_state ON process_instance (state);

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
-- Added in migration 0002 (hot-path query index: load_by_instance / save deletes)
CREATE INDEX IF NOT EXISTS idx_token_instance ON token(instance_id);

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

-- Idempotency keys for start-instance (E2-4, migration 0003).
-- tenant_id is NOT NULL DEFAULT '' (no tenant = ''); a NULL would not
-- participate in the PRIMARY KEY uniqueness.
CREATE TABLE IF NOT EXISTS idempotency_key (
    key TEXT NOT NULL,
    tenant_id TEXT NOT NULL DEFAULT '',
    instance_id TEXT NOT NULL,
    created_at TEXT,
    PRIMARY KEY (key, tenant_id)
);
CREATE INDEX IF NOT EXISTS idx_idempotency_key_instance ON idempotency_key(instance_id);

-- Engine-owned approval records (E5-2, ADR-008, migration 0004).
-- One row per approval-required step visit, subject-unique on
-- (instance_id, node_id, token_id). status: 'Pending' | 'Approved' |
-- 'Rejected' (the decision, flipped exactly once by decide); effect:
-- 'Pending' | 'Applied' (whether the decision's EFFECT — task creation on
-- grant, dead letter + token-terminal on reject — has been applied; the
-- re-drivable unit of ADR-008 §7.1: a pending effect on a decided row is
-- re-driven by a retried same-decision decide and by the startup sweep).
CREATE TABLE IF NOT EXISTS approval_record (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    task_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    effect TEXT NOT NULL DEFAULT 'Pending',
    decided_by TEXT,
    decided_at TEXT,
    note TEXT,
    created_at TEXT,
    CONSTRAINT fk_approval_instance FOREIGN KEY (instance_id)
        REFERENCES process_instance(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_approval_subject
ON approval_record(instance_id, node_id, token_id);
CREATE INDEX IF NOT EXISTS idx_approval_instance
ON approval_record(instance_id);
CREATE INDEX IF NOT EXISTS idx_approval_pending_effect
ON approval_record(effect) WHERE effect = 'Pending';
