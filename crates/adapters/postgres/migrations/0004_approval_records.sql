-- 0004: engine-owned approval records (E5-2, ADR-008).
--
-- One row per approval-required step visit, subject-unique on
-- (instance_id, node_id, token_id): `create` uses
-- INSERT ... ON CONFLICT DO NOTHING, so a re-arrival of the same token at
-- the same node (crash recovery, event re-delivery) keeps the ORIGINAL
-- record instead of creating a second approval requirement.
--
-- `decide` flips a Pending row exactly once:
--   UPDATE ... WHERE id = $1 AND status = 'Pending' RETURNING ...
-- a second (concurrent or retried) decide updates 0 rows and reads back
-- the ORIGINAL outcome — only-once by construction.
--
-- DECISION vs. EFFECT (the re-drivable unit, ADR-008 §7.1): `decide` also
-- sets effect = 'Pending' — the DECISION is durable truth from that commit,
-- while its EFFECT (task creation on grant; dead letter + token-terminal
-- on reject) is applied right after and confirmed by
-- UPDATE ... SET effect = 'Applied' (mark_effect_applied, idempotent).
-- A crash between the two is recoverable: a retried same-decision decide
-- re-drives a pending effect, and the server's startup sweep re-drives
-- every row with status <> 'Pending' AND effect = 'Pending'.
--
-- All statements are idempotent (IF NOT EXISTS / ON CONFLICT), so
-- re-execution is safe. The status column stores the core
-- ApprovalStatus storage strings ('Pending' | 'Approved' | 'Rejected');
-- the effect column stores the core ApprovalEffect strings
-- ('Pending' | 'Applied').
--
-- The `effect` column ships in this migration (0004 is UNRELEASED — it is
-- introduced by the same PR). The trailing ALTER exists so that a database
-- that already ran a pre-amendment 0004 (a local dev database created from
-- an intermediate build) converges when the file is re-applied by hand;
-- for fresh databases the CREATE TABLE already includes the column and the
-- ALTER is a no-op.

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

ALTER TABLE approval_record ADD COLUMN IF NOT EXISTS effect
    TEXT NOT NULL DEFAULT 'Pending';

CREATE UNIQUE INDEX IF NOT EXISTS idx_approval_subject
ON approval_record(instance_id, node_id, token_id);

CREATE INDEX IF NOT EXISTS idx_approval_instance
ON approval_record(instance_id);

-- Startup-sweep lookup: decisions whose effect is still pending.
CREATE INDEX IF NOT EXISTS idx_approval_pending_effect
ON approval_record(effect) WHERE effect = 'Pending';
