-- 0006_definition_tenant.sql — N1-3 tenant-scoped definitions
--
-- Add tenant_id to process_definition. NULL = global/legacy (pre-tenant
-- definitions, visible to all tenants). New definitions deployed with a
-- verified tenant are tenant-scoped. Cross-tenant access is rejected at the
-- API layer (same-shape 404, no existence leak).

ALTER TABLE process_definition ADD COLUMN IF NOT EXISTS tenant_id TEXT;
