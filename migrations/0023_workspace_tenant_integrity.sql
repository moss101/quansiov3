-- A workspace-scoped row belongs to its workspace's tenant (QA-007 finding F-7). Every foreign key
-- to `workspaces` references `id` alone, so a writer in one tenant could store rows naming another
-- tenant's workspace (the row stayed in the writer's tenant, invisible to the other, but the pair
-- (tenant_id, workspace_id) contradicted itself and the id's existence could be probed). Each table
-- that references `workspaces(id)` from a `workspace_id` column also gets a composite key to
-- `workspaces(tenant_id, id)`, so the database refuses the mismatch on every write path.
-- NOT VALID: enforced for every new or changed row; existing rows are not rescanned, so the
-- migration cannot fail on legacy data (a separate VALIDATE CONSTRAINT can follow). Additive.

ALTER TABLE workspaces ADD CONSTRAINT workspaces_tenant_id_id_key UNIQUE (tenant_id, id);

DO $$
DECLARE
    target RECORD;
BEGIN
    FOR target IN
        SELECT DISTINCT n.nspname AS schema_name, c.relname AS table_name
        FROM pg_constraint k
        JOIN pg_class c ON c.oid = k.conrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1]
        WHERE k.contype = 'f'
          AND k.confrelid = 'public.workspaces'::regclass
          AND array_length(k.conkey, 1) = 1
          AND a.attname = 'workspace_id'
          AND c.relname <> 'workspaces'
          AND EXISTS (
              SELECT 1 FROM pg_attribute t
              WHERE t.attrelid = k.conrelid AND t.attname = 'tenant_id' AND NOT t.attisdropped
          )
    LOOP
        EXECUTE format(
            'ALTER TABLE %I.%I ADD CONSTRAINT %I FOREIGN KEY (tenant_id, workspace_id) '
            'REFERENCES public.workspaces (tenant_id, id) NOT VALID',
            target.schema_name,
            target.table_name,
            left(target.table_name, 40) || '_tenant_workspace_fkey'
        );
    END LOOP;
END $$;
