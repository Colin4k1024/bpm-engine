-- 0008_definition_audit.sql — N1-4 definition lifecycle audit trail
--
-- Stores audit records for deploy/activate/deprecate/retire operations.
-- Each record captures actor, version, content summary, reason, timestamp.

CREATE TABLE IF NOT EXISTS definition_audit (
    id BIGSERIAL PRIMARY KEY,
    definition_id TEXT NOT NULL,
    definition_key TEXT NOT NULL,
    version INTEGER NOT NULL,
    operation TEXT NOT NULL,
    actor TEXT,
    content_hash TEXT,
    content_size BIGINT,
    reason TEXT,
    timestamp TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_def_audit_definition ON definition_audit (definition_id);
CREATE INDEX IF NOT EXISTS idx_def_audit_key ON definition_audit (definition_key);
