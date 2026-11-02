-- 0005_definition_lifecycle.sql — ADR-009 definition lifecycle
--
-- Rename existing 'active' status values to 'activated' (the new five-state
-- lifecycle enum: draft/validated/published/activated/deprecated). The status
-- column is TEXT (not an enum type), so new values are additive — no ALTER.
-- 'deprecated' rows are unchanged. Draft/validated/published rows do not
-- exist yet (written by future N1-2/N1-3 endpoints).

UPDATE process_definition SET status = 'activated' WHERE status = 'active';
