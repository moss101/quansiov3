-- Quansio V8.1 deterministic development identities (GOV-007).
--
-- Idempotent: safe to run repeatedly; row identifiers never change.
-- Fixtures live in the dev-only `dev` schema so they can never be mistaken for
-- canonical authority tables (owned by Rust migrations, DOSSIER.md §5). The
-- identifiers are fixed, non-secret ULIDs (DOMAIN.md §1.1) safe to commit; the
-- values are documented in infra/compose/README.md.

CREATE EXTENSION IF NOT EXISTS vector;
CREATE SCHEMA IF NOT EXISTS dev;
CREATE SCHEMA IF NOT EXISTS derived;

CREATE TABLE IF NOT EXISTS dev.seed_identity (
    id            text        PRIMARY KEY
                  CHECK (id ~ '^(tn_|ws_|agt_)[0-9A-HJKMNP-TV-Z]{26}$'),
    kind          text        NOT NULL CHECK (kind IN ('tenant', 'workspace', 'teammate')),
    slug          text        NOT NULL UNIQUE,
    display_name  text        NOT NULL,
    tenant_id     text        NOT NULL,
    workspace_id  text,
    attributes    jsonb       NOT NULL DEFAULT '{}'::jsonb,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);

-- One personal tenant (DOMAIN.md §2), one workspace inside it, one teammate
-- template (AgentDefinition) scoped to that workspace.
INSERT INTO dev.seed_identity (id, kind, slug, display_name, tenant_id, workspace_id, attributes)
VALUES
    (
        'tn_01J8Z3K6F1DEV0000000000TEN',
        'tenant',
        'dev-personal',
        'Quansio Dev Personal Tenant',
        'tn_01J8Z3K6F1DEV0000000000TEN',
        NULL,
        '{"plan": "dev", "personal": true}'::jsonb
    ),
    (
        'ws_01J8Z3K6F1DEV0000000000WKS',
        'workspace',
        'dev-default',
        'Quansio Dev Default Workspace',
        'tn_01J8Z3K6F1DEV0000000000TEN',
        'ws_01J8Z3K6F1DEV0000000000WKS',
        '{"visibility": "private", "membership_role": "admin"}'::jsonb
    ),
    (
        'agt_01J8Z3K6F1DEV0000000000AGT',
        'teammate',
        'dev-teammate',
        'Quansio Dev Teammate Template',
        'tn_01J8Z3K6F1DEV0000000000TEN',
        'ws_01J8Z3K6F1DEV0000000000WKS',
        '{"persona": "dev-template", "default_capabilities": []}'::jsonb
    )
ON CONFLICT (id) DO UPDATE
    SET kind = EXCLUDED.kind,
        slug = EXCLUDED.slug,
        display_name = EXCLUDED.display_name,
        tenant_id = EXCLUDED.tenant_id,
        workspace_id = EXCLUDED.workspace_id,
        attributes = EXCLUDED.attributes,
        updated_at = now();
