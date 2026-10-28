-- 0002: hot-path query indexes.
--
-- Purely additive (CREATE INDEX IF NOT EXISTS): no column adds/drops, no
-- data changes — safe to apply on a live deployment. Chosen from the query
-- patterns in this adapter:
--
--   * process_instance(state): ProcessInstanceStore::list_running scans
--     `WHERE state = 'Running'` on every timer/recovery tick.
--   * token(instance_id): the token primary key is (id, instance_id), so
--     every `load_by_instance` / `DELETE FROM token WHERE instance_id = $1`
--     (the process save path) cannot use it and seq-scans.
--
-- Applied on top of a legacy (inline-migrate era) schema via baseline
-- stamping; see src/migrations.rs and ADR-005.

CREATE INDEX IF NOT EXISTS idx_process_instance_state
ON process_instance(state);

CREATE INDEX IF NOT EXISTS idx_token_instance
ON token(instance_id);
