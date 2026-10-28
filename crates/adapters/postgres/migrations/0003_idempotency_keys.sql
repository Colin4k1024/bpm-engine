-- 0003: start-instance idempotency keys (E2-4).
--
-- Stores the `Idempotency-Key` → instance mapping recorded when a client
-- starts an instance with the header, so a retry of the same POST returns
-- the SAME instance instead of creating a duplicate.
--
-- `tenant_id` is `TEXT NOT NULL DEFAULT ''` rather than NULL: a NULL column
-- inside a PRIMARY KEY does not participate in uniqueness in PostgreSQL
-- (NULLs are distinct), which would silently break the (key, tenant)
-- uniqueness contract. No-tenant keys are stored as the empty string ''.
--
-- All statements are idempotent (IF NOT EXISTS), so re-execution is safe.

CREATE TABLE IF NOT EXISTS idempotency_key (
    key TEXT NOT NULL,
    tenant_id TEXT NOT NULL DEFAULT '',
    instance_id TEXT NOT NULL,
    created_at TEXT,
    PRIMARY KEY (key, tenant_id)
);

CREATE INDEX IF NOT EXISTS idx_idempotency_key_instance
ON idempotency_key(instance_id);
