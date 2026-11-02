-- 0007: persistent event inbox for message/signal delivery (N1-2, ADR-010).
--
-- Stores pending message/signal deliveries that arrive before a catch
-- token is ready, or while a catch token is paused (Suspended). Mirrors
-- the TimerStore shape: insert + list_pending + mark_consumed.
--
-- Idempotency: `insert` uses INSERT ... ON CONFLICT (dedup_key) DO NOTHING,
-- so re-delivering the same message/signal (same dedup_key) keeps the
-- ORIGINAL record instead of creating a duplicate.
--
-- Status values are lowercase snake_case: 'pending', 'consumed', 'expired'.
-- Delivery types: 'message', 'signal'.
--
-- All statements are idempotent (IF NOT EXISTS), so re-execution is safe.

CREATE TABLE IF NOT EXISTS event_inbox (
    id TEXT PRIMARY KEY,
    tenant_id TEXT,
    target_instance_id TEXT,
    delivery_type TEXT NOT NULL,
    name TEXT NOT NULL,
    correlation JSONB NOT NULL DEFAULT '{}',
    payload JSONB NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending',
    dedup_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_event_inbox_pending ON event_inbox (name, tenant_id, status) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_event_inbox_instance ON event_inbox (target_instance_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_event_inbox_dedup ON event_inbox (dedup_key);
