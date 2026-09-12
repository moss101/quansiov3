-- Quansio V8.1 authoritative schema, migration 0001 (CORE-001).
--
-- Authority: DOMAIN.md §2–§13 (entities) and §9/§19 (events, evidence). DOSSIER.md §9
-- (PostgreSQL is the authoritative server/control/runtime database) and §5 (ownership).
--
-- Conventions
--   * Canonical ids are TEXT carrying the DOMAIN.md §1 prefix, checked per table.
--   * Every tenant table carries tenant_id; workspace-scoped tables also carry
--     workspace_id. Timestamps are TIMESTAMPTZ (UTC); names/values are DOMAIN.md's.
--   * Row-level security is ENABLED and FORCED on every tenant table, so a query
--     without `SET LOCAL quansio.tenant_id = '<tn_…>'` returns zero rows. Application
--     scoping remains mandatory (defence in depth, DOSSIER.md §9).
--   * Rebuildable search structures live in the separate `derived` schema (pgvector),
--     never mixed with authoritative tables.
--
-- Rollback policy: forward migrations with forward-fix. A release never ships a
-- destructive `down`; recovery is a new forward migration plus restore from backup
-- (OPS-005). `migrations/README.md` records the policy and the migration inventory.

CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS vector;

CREATE SCHEMA IF NOT EXISTS derived;

-- --------------------------------------------------------------------------
-- Identity and tenancy (DOMAIN.md §2)
-- --------------------------------------------------------------------------

CREATE TABLE tenants (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'tn\_%'),
    name          TEXT NOT NULL,
    personal      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'usr\_%'),
    primary_email TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active'
                  CHECK (status IN ('active', 'suspended', 'deleted')),
    auth_methods  JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE tenant_memberships (
    id         TEXT PRIMARY KEY CHECK (id LIKE 'tm\_%'),
    tenant_id  TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'billing', 'member')),
    status     TEXT NOT NULL DEFAULT 'active'
               CHECK (status IN ('invited', 'active', 'suspended', 'removed')),
    invited_by TEXT REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, user_id)
);

CREATE TABLE workspaces (
    id         TEXT PRIMARY KEY CHECK (id LIKE 'ws\_%'),
    tenant_id  TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX workspaces_tenant_idx ON workspaces (tenant_id, created_at DESC);

CREATE TABLE workspace_memberships (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'wm\_%'),
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role         TEXT NOT NULL CHECK (role IN ('admin', 'editor', 'approver', 'viewer')),
    status       TEXT NOT NULL DEFAULT 'active'
                 CHECK (status IN ('invited', 'active', 'suspended', 'removed')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, user_id)
);

CREATE TABLE service_principals (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'sp\_%'),
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    scopes       JSONB NOT NULL DEFAULT '[]'::jsonb,
    key_hash     TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'active'
                 CHECK (status IN ('invited', 'active', 'suspended', 'removed')),
    last_used_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE secret_handles (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'sec\_%'),
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    provider     TEXT NOT NULL,
    label        TEXT NOT NULL,
    -- Envelope-encrypted material only; raw secrets never reach the database in plaintext.
    ciphertext   BYTEA,
    data_key_ref TEXT,
    status       TEXT NOT NULL DEFAULT 'active'
                 CHECK (status IN ('active', 'revoked', 'expired')),
    last_used_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE connector_instances (
    id                 TEXT PRIMARY KEY CHECK (id LIKE 'cnx\_%'),
    tenant_id          TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id       TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    connector_id       TEXT NOT NULL,
    display_name       TEXT NOT NULL,
    credential_handle  TEXT REFERENCES secret_handles(id),
    metadata           JSONB NOT NULL DEFAULT '{}'::jsonb,
    status             TEXT NOT NULL DEFAULT 'connected'
                       CHECK (status IN ('connected', 'degraded', 'revoked')),
    last_health_at     TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------------------------------------
-- Conversation (DOMAIN.md §3)
-- --------------------------------------------------------------------------

CREATE TABLE threads (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'thr\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id     TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind             TEXT NOT NULL CHECK (kind IN ('direct', 'group', 'objective')),
    title            TEXT,
    objective_id     TEXT,
    archived_at      TIMESTAMPTZ,
    last_activity_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX threads_workspace_idx ON threads (workspace_id, last_activity_at DESC);

CREATE TABLE thread_participants (
    id         TEXT PRIMARY KEY CHECK (id LIKE 'thp\_%'),
    tenant_id  TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    thread_id  TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('user', 'teammate', 'worker')),
    principal_id TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    joined_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    left_at    TIMESTAMPTZ,
    UNIQUE (thread_id, kind, principal_id)
);

CREATE TABLE messages (
    id             TEXT PRIMARY KEY CHECK (id LIKE 'msg\_%'),
    tenant_id      TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id   TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    thread_id      TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    seq            BIGINT NOT NULL,
    author_kind    TEXT NOT NULL CHECK (author_kind IN ('user', 'agent', 'system')),
    author_id      TEXT NOT NULL,
    run_id         TEXT,
    turn_id        TEXT,
    content_blocks JSONB NOT NULL DEFAULT '[]'::jsonb,
    attachments    JSONB NOT NULL DEFAULT '[]'::jsonb,
    reply_to       TEXT REFERENCES messages(id),
    reactions      JSONB NOT NULL DEFAULT '[]'::jsonb,
    mentions       JSONB NOT NULL DEFAULT '[]'::jsonb,
    trust_level    SMALLINT NOT NULL DEFAULT 2,
    edited_at      TIMESTAMPTZ,
    deleted_at     TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (thread_id, seq)
);
CREATE INDEX messages_thread_idx ON messages (thread_id, seq);

CREATE TABLE questions (
    id          TEXT PRIMARY KEY CHECK (id LIKE 'q\_%'),
    tenant_id   TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    run_id      TEXT NOT NULL,
    step_id     TEXT,
    thread_id   TEXT REFERENCES threads(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('free_text', 'single_choice', 'multi_choice', 'confirm')),
    prompt      TEXT NOT NULL,
    options     JSONB NOT NULL DEFAULT '[]'::jsonb,
    required    BOOLEAN NOT NULL DEFAULT TRUE,
    status      TEXT NOT NULL DEFAULT 'open'
                CHECK (status IN ('open', 'answered', 'expired', 'cancelled')),
    answer      JSONB,
    expires_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX questions_open_idx ON questions (run_id) WHERE status = 'open';

-- --------------------------------------------------------------------------
-- Work graph (DOMAIN.md §4)
-- --------------------------------------------------------------------------

CREATE TABLE work_nodes (
    id                        TEXT PRIMARY KEY CHECK (id LIKE 'wn\_%'),
    tenant_id                 TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id              TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind                      TEXT NOT NULL CHECK (kind IN ('objective', 'task', 'subtask', 'wait', 'milestone')),
    title                     TEXT NOT NULL,
    description               TEXT,
    parent_id                 TEXT REFERENCES work_nodes(id) ON DELETE CASCADE,
    status                    TEXT NOT NULL DEFAULT 'draft'
                              CHECK (status IN ('draft', 'ready', 'blocked', 'in_progress', 'waiting',
                                                'verifying', 'done', 'failed', 'cancelled')),
    owner_agent_thread_id     TEXT,
    created_by                JSONB NOT NULL,
    completion_contract       JSONB NOT NULL DEFAULT '{}'::jsonb,
    capability_needs          JSONB NOT NULL DEFAULT '[]'::jsonb,
    budget_id                 TEXT,
    priority                  SMALLINT NOT NULL DEFAULT 1 CHECK (priority BETWEEN 0 AND 3),
    revision                  BIGINT NOT NULL DEFAULT 1,
    thread_id                 TEXT REFERENCES threads(id) ON DELETE SET NULL,
    origin                    TEXT NOT NULL DEFAULT 'user'
                              CHECK (origin IN ('user', 'plan_proposal', 'routine', 'pack')),
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX work_nodes_workspace_status_idx ON work_nodes (workspace_id, status);
CREATE INDEX work_nodes_parent_idx ON work_nodes (parent_id);

CREATE TABLE work_edges (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'we\_%'),
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id  TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    from_node_id  TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    to_node_id    TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('depends_on', 'parent_of', 'produces_artifact',
                                                'verified_by', 'blocked_by')),
    revision      BIGINT NOT NULL DEFAULT 1,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (from_node_id, to_node_id, kind),
    CHECK (from_node_id <> to_node_id)
);
CREATE INDEX work_edges_to_idx ON work_edges (to_node_id);

-- --------------------------------------------------------------------------
-- Runtime (DOMAIN.md §5)
-- --------------------------------------------------------------------------

CREATE TABLE agent_threads (
    id                       TEXT PRIMARY KEY CHECK (id LIKE 'ath\_%'),
    tenant_id                TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id             TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    agent_kind               TEXT NOT NULL CHECK (agent_kind IN ('teammate', 'worker')),
    definition_id            TEXT,
    parent_id                TEXT REFERENCES agent_threads(id) ON DELETE SET NULL,
    work_node_id             TEXT REFERENCES work_nodes(id) ON DELETE SET NULL,
    capability_projection_id TEXT,
    generation               BIGINT NOT NULL DEFAULT 1,
    status                   TEXT NOT NULL DEFAULT 'PROVISIONED'
                             CHECK (status IN ('PROVISIONED', 'ACTIVE', 'SUSPENDED', 'HANDING_OFF',
                                               'HANDED_OFF', 'JOINING', 'JOINED', 'TERMINATED')),
    mailbox_cursor           TEXT,
    execution_target_id      TEXT,
    budget_id                TEXT,
    suspended_reason         TEXT,
    handoff_to_agent_thread_id TEXT REFERENCES agent_threads(id),
    handoff_at               TIMESTAMPTZ,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX agent_threads_workspace_status_idx ON agent_threads (workspace_id, status);

CREATE TABLE agent_graph_edges (
    id                     TEXT PRIMARY KEY CHECK (id LIKE 'age\_%'),
    tenant_id              TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    parent_agent_thread_id TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    child_agent_thread_id  TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    work_node_id           TEXT REFERENCES work_nodes(id) ON DELETE SET NULL,
    delegation_capability_id TEXT,
    delegated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    joined_at              TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (parent_agent_thread_id, child_agent_thread_id)
);

CREATE TABLE runs (
    id                  TEXT PRIMARY KEY CHECK (id LIKE 'run\_%'),
    tenant_id           TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    work_node_id        TEXT NOT NULL REFERENCES work_nodes(id) ON DELETE CASCADE,
    agent_thread_id     TEXT NOT NULL REFERENCES agent_threads(id) ON DELETE CASCADE,
    generation          BIGINT NOT NULL DEFAULT 1,
    status              TEXT NOT NULL DEFAULT 'CREATED'
                        CHECK (status IN ('CREATED', 'QUEUED', 'RUNNING', 'WAITING_APPROVAL',
                                          'WAITING_QUESTION', 'WAITING_EVENT', 'WAITING_TIMER',
                                          'WAITING_CHILD', 'WAITING_TAKEOVER', 'VERIFYING',
                                          'SUCCEEDED', 'FAILED', 'CANCELLED', 'BLOCKED_UNRECOVERABLE',
                                          'SUSPENDED')),
    trigger_kind        TEXT NOT NULL
                        CHECK (trigger_kind IN ('message', 'routine', 'wake', 'child_result', 'manual')),
    trigger_ref         TEXT,
    current_turn_id     TEXT,
    budget_snapshot     JSONB NOT NULL DEFAULT '{}'::jsonb,
    terminal_reason     TEXT,
    execution_target_id TEXT,
    started_at          TIMESTAMPTZ,
    ended_at            TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX runs_agent_thread_idx ON runs (agent_thread_id, created_at DESC);
CREATE INDEX runs_status_idx ON runs (status) WHERE status NOT IN ('SUCCEEDED', 'FAILED', 'CANCELLED');

CREATE TABLE turns (
    id                    TEXT PRIMARY KEY CHECK (id LIKE 'trn\_%'),
    tenant_id             TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id                TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    seq                   INTEGER NOT NULL,
    input_kind            TEXT NOT NULL,
    input_ref             TEXT,
    context_projection_id TEXT,
    status                TEXT NOT NULL DEFAULT 'active'
                          CHECK (status IN ('active', 'completed', 'aborted')),
    step_count            INTEGER NOT NULL DEFAULT 0,
    token_ledger          JSONB NOT NULL DEFAULT '{}'::jsonb,
    started_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at              TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (run_id, seq)
);

CREATE TABLE steps (
    id          TEXT PRIMARY KEY CHECK (id LIKE 'stp\_%'),
    tenant_id   TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    turn_id     TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('model_call', 'tool_call', 'delegate', 'wait',
                                              'verify', 'checkpoint', 'compact')),
    status      TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'dispatched', 'completed', 'failed', 'cancelled', 'unknown')),
    ref         TEXT,
    evidence_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    effect_id   TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (turn_id, seq)
);

CREATE TABLE attempts (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'att\_%'),
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    step_id       TEXT NOT NULL REFERENCES steps(id) ON DELETE CASCADE,
    seq           INTEGER NOT NULL,
    generation    BIGINT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'started'
                  CHECK (status IN ('started', 'succeeded', 'failed', 'timed_out', 'fenced')),
    error         JSONB,
    dispatched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at   TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (step_id, seq)
);

CREATE TABLE protocol_states (
    id                     TEXT PRIMARY KEY CHECK (id LIKE 'pst\_%'),
    tenant_id              TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id                 TEXT NOT NULL UNIQUE REFERENCES runs(id) ON DELETE CASCADE,
    generation             BIGINT NOT NULL,
    pending_model_call     JSONB,
    pending_tool_calls     JSONB NOT NULL DEFAULT '[]'::jsonb,
    pending_approvals      JSONB NOT NULL DEFAULT '[]'::jsonb,
    open_questions         JSONB NOT NULL DEFAULT '[]'::jsonb,
    waits                  JSONB NOT NULL DEFAULT '[]'::jsonb,
    browser_control        JSONB,
    terminal_sessions      JSONB NOT NULL DEFAULT '[]'::jsonb,
    child_agent_threads    JSONB NOT NULL DEFAULT '[]'::jsonb,
    cancellation_requested BOOLEAN NOT NULL DEFAULT FALSE,
    cancellation_at        TIMESTAMPTZ,
    last_compaction_epoch_id TEXT,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE compaction_epochs (
    id                       TEXT PRIMARY KEY CHECK (id LIKE 'cep\_%'),
    tenant_id                TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    thread_id                TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    run_id                   TEXT REFERENCES runs(id) ON DELETE SET NULL,
    seq                      INTEGER NOT NULL,
    source_from_sequence     BIGINT NOT NULL,
    source_to_sequence       BIGINT NOT NULL,
    summary_artifact_id      TEXT,
    token_estimate           BIGINT NOT NULL DEFAULT 0,
    status                   TEXT NOT NULL DEFAULT 'pending'
                             CHECK (status IN ('pending', 'installed', 'rejected_stale')),
    created_by_model_route_id TEXT,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (thread_id, seq),
    CHECK (source_to_sequence >= source_from_sequence)
);

CREATE TABLE checkpoints (
    id                  TEXT PRIMARY KEY CHECK (id LIKE 'ckp\_%'),
    tenant_id           TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    execution_target_id TEXT NOT NULL,
    run_id              TEXT REFERENCES runs(id) ON DELETE SET NULL,
    kind                TEXT NOT NULL CHECK (kind IN ('workspace_files', 'browser_session',
                                                     'terminal', 'full')),
    storage_ref         TEXT NOT NULL,
    generation          BIGINT NOT NULL,
    size_bytes          BIGINT NOT NULL DEFAULT 0,
    restore_policy      TEXT,
    expires_at          TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------------------------------------
-- Capability, policy, approvals and effects (DOMAIN.md §6–§7)
-- --------------------------------------------------------------------------

CREATE TABLE capability_projections (
    id              TEXT PRIMARY KEY CHECK (id LIKE 'cap\_%'),
    tenant_id       TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id    TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    subject_kind    TEXT NOT NULL CHECK (subject_kind IN ('run', 'agent_thread', 'tool_call')),
    subject_id      TEXT NOT NULL,
    inputs          JSONB NOT NULL DEFAULT '[]'::jsonb,
    grants          JSONB NOT NULL DEFAULT '[]'::jsonb,
    inputs_digest   TEXT NOT NULL,
    computed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX capability_projections_subject_idx ON capability_projections (subject_kind, subject_id);

CREATE TABLE policies (
    id                        TEXT PRIMARY KEY CHECK (id LIKE 'pol\_%'),
    tenant_id                 TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id              TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    scope                     TEXT NOT NULL CHECK (scope IN ('tenant', 'workspace')),
    rules                     JSONB NOT NULL DEFAULT '[]'::jsonb,
    question_default_ttl_seconds BIGINT NOT NULL DEFAULT 86400,
    approval_default_ttl_seconds BIGINT NOT NULL DEFAULT 3600,
    max_plan_nodes            INTEGER NOT NULL DEFAULT 25,
    version                   INTEGER NOT NULL DEFAULT 1,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A workspace policy can only narrow the tenant policy (DOSSIER.md §10).
    CHECK ((scope = 'tenant' AND workspace_id IS NULL) OR (scope = 'workspace' AND workspace_id IS NOT NULL))
);

CREATE TABLE user_rules (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'rule\_%'),
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    effect_class TEXT NOT NULL,
    resource     JSONB NOT NULL,
    decision     TEXT NOT NULL CHECK (decision IN ('ask', 'always', 'never')),
    expires_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE policy_decisions (
    id                      TEXT PRIMARY KEY CHECK (id LIKE 'pdc\_%'),
    tenant_id               TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id                  TEXT REFERENCES runs(id) ON DELETE SET NULL,
    effect_id               TEXT,
    capability_projection_id TEXT,
    decision                TEXT NOT NULL CHECK (decision IN ('allow', 'ask', 'deny')),
    reason                  TEXT,
    inputs_digest           TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE approval_requests (
    id                      TEXT PRIMARY KEY CHECK (id LIKE 'apr\_%'),
    tenant_id               TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id            TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    run_id                  TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    effect_id               TEXT NOT NULL,
    requested_of            JSONB NOT NULL DEFAULT '[]'::jsonb,
    summary                 TEXT NOT NULL,
    consequence_preview     JSONB NOT NULL DEFAULT '{}'::jsonb,
    params_digest           TEXT NOT NULL,
    capability_projection_id TEXT NOT NULL,
    status                  TEXT NOT NULL DEFAULT 'requested'
                            CHECK (status IN ('requested', 'granted', 'denied', 'expired', 'superseded')),
    expires_at              TIMESTAMPTZ NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX approval_requests_run_idx ON approval_requests (run_id, status);

CREATE TABLE approval_receipts (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'rcp\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    request_id       TEXT NOT NULL REFERENCES approval_requests(id) ON DELETE CASCADE,
    effect_id        TEXT NOT NULL,
    approver_user_id TEXT NOT NULL REFERENCES users(id),
    params_digest    TEXT NOT NULL,
    scope            TEXT NOT NULL DEFAULT 'single_use' CHECK (scope = 'single_use'),
    generation       BIGINT NOT NULL,
    granted_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ NOT NULL,
    signature        TEXT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE effect_records (
    id                      TEXT PRIMARY KEY CHECK (id LIKE 'eff\_%'),
    tenant_id               TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id            TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    run_id                  TEXT REFERENCES runs(id) ON DELETE SET NULL,
    step_id                 TEXT REFERENCES steps(id) ON DELETE SET NULL,
    tool_call_id            TEXT,
    effect_class            TEXT NOT NULL,
    tier                    SMALLINT NOT NULL CHECK (tier BETWEEN 0 AND 4),
    resource                JSONB NOT NULL,
    params_digest           TEXT NOT NULL,
    idempotency_key         TEXT NOT NULL,
    capability_projection_id TEXT NOT NULL,
    policy_decision_id      TEXT,
    approval_receipt_id     TEXT REFERENCES approval_receipts(id),
    status                  TEXT NOT NULL DEFAULT 'PROPOSED'
                            CHECK (status IN ('PROPOSED', 'AUTHORIZED', 'RESERVED', 'DISPATCHED',
                                              'SETTLED_SUCCESS', 'SETTLED_FAILED', 'OUTCOME_UNKNOWN',
                                              'RECONCILING', 'RECONCILED_SUCCESS', 'RECONCILED_FAILED',
                                              'RECONCILIATION_MANUAL', 'DENIED', 'EXPIRED', 'CANCELLED')),
    dispatch_token          TEXT,
    target_kind             TEXT CHECK (target_kind IN ('server', 'qworkerd', 'browser', 'adapter')),
    target_id               TEXT,
    outcome                 JSONB,
    reconciliation          JSONB,
    generation              BIGINT NOT NULL,
    reserved_at             TIMESTAMPTZ,
    dispatched_at           TIMESTAMPTZ,
    settled_at              TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- No retry while an effect with the same key is in flight or unresolved (DOMAIN.md §7.2).
CREATE UNIQUE INDEX effect_records_inflight_key_idx
    ON effect_records (tenant_id, effect_class, idempotency_key)
    WHERE status IN ('RESERVED', 'DISPATCHED', 'OUTCOME_UNKNOWN', 'RECONCILING');
CREATE INDEX effect_records_run_idx ON effect_records (run_id, created_at DESC);
CREATE INDEX effect_records_unsettled_idx ON effect_records (tenant_id, created_at)
    WHERE status IN ('DISPATCHED', 'OUTCOME_UNKNOWN', 'RECONCILING');

CREATE TABLE tool_calls (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'tc\_%'),
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    step_id       TEXT NOT NULL REFERENCES steps(id) ON DELETE CASCADE,
    tool_name     TEXT NOT NULL,
    args          JSONB NOT NULL,
    args_digest   TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'proposed'
                  CHECK (status IN ('proposed', 'validated', 'rejected', 'dispatched', 'completed',
                                    'failed', 'unknown')),
    result        JSONB,
    effect_id     TEXT REFERENCES effect_records(id),
    trust_level   SMALLINT NOT NULL DEFAULT 5,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX tool_calls_step_idx ON tool_calls (step_id);

-- Command idempotency: a duplicate command returns the original result (DOMAIN.md §1.2).
CREATE TABLE commands (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'cmd\_%'),
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    command_name TEXT NOT NULL,
    command_id   TEXT NOT NULL,
    params_digest TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'accepted'
                 CHECK (status IN ('accepted', 'applied', 'rejected')),
    result       JSONB,
    error        JSONB,
    correlation_id TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, command_id)
);

-- --------------------------------------------------------------------------
-- Events and outbox (DOMAIN.md §9)
-- --------------------------------------------------------------------------

CREATE TABLE runtime_events (
    id                TEXT PRIMARY KEY CHECK (id LIKE 'evt\_%'),
    tenant_id         TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id      TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    sequence          BIGINT NOT NULL,
    aggregate_type    TEXT NOT NULL,
    aggregate_id      TEXT NOT NULL,
    aggregate_version BIGINT NOT NULL,
    type              TEXT NOT NULL,
    schema_version    TEXT NOT NULL DEFAULT 'v1',
    occurred_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    command_id        TEXT,
    correlation_id    TEXT,
    causation_id      TEXT,
    actor             JSONB NOT NULL,
    generation        BIGINT,
    payload           JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, sequence)
);
CREATE INDEX runtime_events_aggregate_idx ON runtime_events (aggregate_type, aggregate_id, sequence);
CREATE INDEX runtime_events_type_idx ON runtime_events (tenant_id, type, sequence);

-- Written in the same transaction as the mutation; the publisher is at-least-once and
-- deduplicated by Nats-Msg-Id = event_id.
CREATE TABLE event_outbox (
    event_id     TEXT PRIMARY KEY,
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    subject      TEXT NOT NULL,
    payload      JSONB NOT NULL,
    published_at TIMESTAMPTZ,
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX event_outbox_unpublished_idx ON event_outbox (created_at) WHERE published_at IS NULL;

CREATE TABLE event_cursors (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'ecr\_%'),
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    stream_id     TEXT NOT NULL,
    consumer      TEXT NOT NULL,
    sequence      BIGINT NOT NULL DEFAULT 0,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, stream_id, consumer)
);

-- --------------------------------------------------------------------------
-- Execution fabric (DOMAIN.md §8)
-- --------------------------------------------------------------------------

CREATE TABLE execution_targets (
    id                  TEXT PRIMARY KEY CHECK (id LIKE 'tgt\_%'),
    tenant_id           TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    target_class        TEXT NOT NULL CHECK (target_class IN ('persistent_workspace_computer', 'isolated_task_runtime')),
    substrate           TEXT NOT NULL CHECK (substrate IN ('cloud_microvm', 'local_capsule_macos',
                                                          'local_capsule_windows', 'windows_native',
                                                          'customer_private_worker')),
    status              TEXT NOT NULL DEFAULT 'REQUESTED'
                        CHECK (status IN ('REQUESTED', 'PROVISIONING', 'READY', 'BUSY', 'DRAINING',
                                          'STOPPED', 'SNAPSHOTTED', 'DESTROYED', 'FAILED', 'REPLACING')),
    desired_state       TEXT,
    observed_state      TEXT,
    image_digest        TEXT,
    lease_id            TEXT,
    generation          BIGINT NOT NULL DEFAULT 1,
    network_policy_id   TEXT,
    resources           JSONB NOT NULL DEFAULT '{}'::jsonb,
    endpoint_ref        TEXT,
    owner_kind          TEXT NOT NULL DEFAULT 'workspace' CHECK (owner_kind IN ('workspace', 'user', 'run')),
    owner_ref           TEXT,
    last_heartbeat_at   TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX execution_targets_workspace_idx ON execution_targets (workspace_id, status);

CREATE TABLE leases (
    id                 TEXT PRIMARY KEY CHECK (id LIKE 'lse\_%'),
    tenant_id          TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    target_id          TEXT NOT NULL REFERENCES execution_targets(id) ON DELETE CASCADE,
    holder_controller_id TEXT NOT NULL,
    holder_generation  BIGINT NOT NULL,
    acquired_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    renewed_at         TIMESTAMPTZ,
    expires_at         TIMESTAMPTZ NOT NULL,
    status             TEXT NOT NULL DEFAULT 'held'
                       CHECK (status IN ('held', 'released', 'expired', 'revoked')),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX leases_active_target_idx ON leases (target_id) WHERE status = 'held';

CREATE TABLE browser_sessions (
    id              TEXT PRIMARY KEY CHECK (id LIKE 'bsn\_%'),
    tenant_id       TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    target_id       TEXT NOT NULL REFERENCES execution_targets(id) ON DELETE CASCADE,
    run_id          TEXT REFERENCES runs(id) ON DELETE SET NULL,
    profile_ref     TEXT,
    control_holder  TEXT NOT NULL DEFAULT 'agent' CHECK (control_holder IN ('agent', 'user', 'none')),
    control_since   TIMESTAMPTZ,
    current_url     TEXT,
    tabs            JSONB NOT NULL DEFAULT '[]'::jsonb,
    screencast      JSONB NOT NULL DEFAULT '{}'::jsonb,
    checkpoint_id   TEXT REFERENCES checkpoints(id),
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'paused_takeover', 'paused_policy', 'closed')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE terminal_sessions (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'tsn\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    target_id        TEXT NOT NULL REFERENCES execution_targets(id) ON DELETE CASCADE,
    run_id           TEXT REFERENCES runs(id) ON DELETE SET NULL,
    pty_ref          TEXT,
    cursor           TEXT,
    status           TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed', 'lost')),
    last_command_id  TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------------------------------------
-- Artifacts, evidence (DOMAIN.md §10)
-- --------------------------------------------------------------------------

CREATE TABLE artifacts (
    id                 TEXT PRIMARY KEY CHECK (id LIKE 'art\_%'),
    tenant_id          TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id       TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    kind               TEXT NOT NULL CHECK (kind IN ('document', 'spreadsheet', 'presentation', 'code',
                                                     'data', 'image', 'audio', 'video', 'archive', 'other')),
    title              TEXT NOT NULL,
    role               TEXT,
    origin             JSONB NOT NULL,
    current_version_id TEXT,
    retention          JSONB NOT NULL DEFAULT '{}'::jsonb,
    deleted_at         TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX artifacts_workspace_idx ON artifacts (workspace_id, created_at DESC);

CREATE TABLE artifact_versions (
    id                TEXT PRIMARY KEY CHECK (id LIKE 'artv\_%'),
    tenant_id         TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    artifact_id       TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    seq               INTEGER NOT NULL,
    content_digest    TEXT NOT NULL,
    size_bytes        BIGINT NOT NULL DEFAULT 0,
    media_type        TEXT NOT NULL,
    object_key        TEXT NOT NULL,
    produced_by_run_id  TEXT REFERENCES runs(id) ON DELETE SET NULL,
    produced_by_step_id TEXT REFERENCES steps(id) ON DELETE SET NULL,
    produced_by_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    parent_version_id TEXT REFERENCES artifact_versions(id),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (artifact_id, seq)
);

CREATE TABLE artifact_grants (
    id         TEXT PRIMARY KEY CHECK (id LIKE 'arg\_%'),
    tenant_id  TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    principal  TEXT NOT NULL,
    level      TEXT NOT NULL CHECK (level IN ('read', 'write')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (artifact_id, principal)
);

-- Immutable execution proof; rows are never updated after insert.
CREATE TABLE evidence (
    id                TEXT PRIMARY KEY CHECK (id LIKE 'evd\_%'),
    tenant_id         TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id      TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    run_id            TEXT REFERENCES runs(id) ON DELETE SET NULL,
    step_id           TEXT REFERENCES steps(id) ON DELETE SET NULL,
    effect_id         TEXT REFERENCES effect_records(id) ON DELETE SET NULL,
    kind              TEXT NOT NULL CHECK (kind IN ('tool_output', 'screenshot', 'dom_snapshot', 'log',
                                                    'test_result', 'http_exchange', 'diff', 'digest',
                                                    'external_ref')),
    content_digest    TEXT NOT NULL,
    object_key        TEXT,
    inline_summary    TEXT,
    captured_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    captured_by_kind  TEXT NOT NULL,
    captured_by_id    TEXT NOT NULL,
    redaction_applied BOOLEAN NOT NULL DEFAULT FALSE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX evidence_run_idx ON evidence (run_id, created_at);

CREATE TABLE evidence_bundles (
    id                  TEXT PRIMARY KEY CHECK (id LIKE 'evb\_%'),
    tenant_id           TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id              TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    artifact_version_id TEXT REFERENCES artifact_versions(id) ON DELETE SET NULL,
    claims              JSONB NOT NULL DEFAULT '[]'::jsonb,
    sources             JSONB NOT NULL DEFAULT '[]'::jsonb,
    coverage            DOUBLE PRECISION,
    verification        JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --------------------------------------------------------------------------
-- Intelligence plane state (DOMAIN.md §11)
-- --------------------------------------------------------------------------

CREATE TABLE context_projections (
    id                    TEXT PRIMARY KEY CHECK (id LIKE 'ctx\_%'),
    tenant_id             TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id                TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    turn_id               TEXT NOT NULL,
    segments              JSONB NOT NULL DEFAULT '[]'::jsonb,
    token_ledger          JSONB NOT NULL DEFAULT '{}'::jsonb,
    degradation           JSONB NOT NULL DEFAULT '[]'::jsonb,
    search_program_id     TEXT,
    policy_snapshot_digest TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX context_projections_run_idx ON context_projections (run_id, created_at DESC);

CREATE TABLE model_routes (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'mr\_%'),
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    request_class TEXT NOT NULL,
    provider      TEXT NOT NULL,
    model_id      TEXT NOT NULL,
    config        JSONB NOT NULL DEFAULT '{}'::jsonb,
    dlp_profile   TEXT,
    fallbacks     JSONB NOT NULL DEFAULT '[]'::jsonb,
    cost_class    TEXT,
    chosen_by     TEXT NOT NULL,
    selected_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE model_calls (
    id                    TEXT PRIMARY KEY CHECK (id LIKE 'mcl\_%'),
    tenant_id             TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    run_id                TEXT REFERENCES runs(id) ON DELETE SET NULL,
    turn_id               TEXT,
    step_id               TEXT REFERENCES steps(id) ON DELETE SET NULL,
    route_id              TEXT REFERENCES model_routes(id),
    context_projection_id TEXT,
    call_id               TEXT NOT NULL,
    status                TEXT NOT NULL DEFAULT 'started'
                          CHECK (status IN ('started', 'completed', 'failed', 'cancelled', 'refused')),
    stop_reason           TEXT,
    usage                 JSONB NOT NULL DEFAULT '{}'::jsonb,
    latency_ms            BIGINT,
    cost_estimate_minor_units BIGINT,
    error                 JSONB,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, call_id)
);

CREATE TABLE knowledge_entries (
    id            TEXT PRIMARY KEY CHECK (id LIKE 'kn\_%'),
    tenant_id     TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id  TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    scope         TEXT NOT NULL CHECK (scope IN ('tenant', 'workspace', 'pack')),
    kind          TEXT NOT NULL,
    content_ref   TEXT,
    content       JSONB,
    provenance    JSONB NOT NULL DEFAULT '[]'::jsonb,
    confidence    DOUBLE PRECISION NOT NULL DEFAULT 0.5 CHECK (confidence BETWEEN 0 AND 1),
    version       INTEGER NOT NULL DEFAULT 1,
    status        TEXT NOT NULL DEFAULT 'candidate'
                  CHECK (status IN ('candidate', 'verified', 'active', 'superseded', 'quarantined', 'deleted')),
    superseded_by TEXT REFERENCES knowledge_entries(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX knowledge_entries_scope_idx ON knowledge_entries (tenant_id, workspace_id, status);

CREATE TABLE memory_entries (
    id              TEXT PRIMARY KEY CHECK (id LIKE 'mem\_%'),
    tenant_id       TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id    TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    scope           TEXT NOT NULL CHECK (scope IN ('user', 'workspace', 'teammate')),
    subject_ref     TEXT NOT NULL,
    content         TEXT NOT NULL,
    provenance_kind TEXT NOT NULL CHECK (provenance_kind IN ('explicit_user', 'verified_run')),
    provenance_ref  TEXT,
    confidence      DOUBLE PRECISION NOT NULL DEFAULT 0.5 CHECK (confidence BETWEEN 0 AND 1),
    status          TEXT NOT NULL DEFAULT 'candidate' CHECK (status IN ('candidate', 'active', 'deleted')),
    last_used_at    TIMESTAMPTZ,
    expires_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX memory_entries_subject_idx ON memory_entries (tenant_id, scope, subject_ref, status);

CREATE TABLE skills (
    id                       TEXT PRIMARY KEY CHECK (id LIKE 'skl\_%'),
    tenant_id                TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    scope                    TEXT NOT NULL CHECK (scope IN ('tenant', 'workspace', 'pack')),
    name                     TEXT NOT NULL,
    owner                    TEXT NOT NULL,
    current_active_version_id TEXT,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

CREATE TABLE skill_versions (
    id         TEXT PRIMARY KEY CHECK (id LIKE 'sklv\_%'),
    tenant_id  TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    skill_id   TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    semver     TEXT NOT NULL,
    manifest   JSONB NOT NULL DEFAULT '{}'::jsonb,
    provenance TEXT,
    status     TEXT NOT NULL DEFAULT 'draft'
               CHECK (status IN ('draft', 'candidate', 'evaluating', 'approved', 'active',
                                 'deprecated', 'retired', 'rejected')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (skill_id, semver)
);

CREATE TABLE capability_packs (
    id                           TEXT PRIMARY KEY CHECK (id LIKE 'pck\_%'),
    tenant_id                    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name                         TEXT NOT NULL,
    current_published_version_id TEXT,
    created_at                   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

CREATE TABLE capability_pack_versions (
    id                    TEXT PRIMARY KEY CHECK (id LIKE 'pckv\_%'),
    tenant_id             TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    pack_id               TEXT NOT NULL REFERENCES capability_packs(id) ON DELETE CASCADE,
    semver                TEXT NOT NULL,
    contents              JSONB NOT NULL DEFAULT '{}'::jsonb,
    provenance            TEXT,
    evidence_requirements JSONB NOT NULL DEFAULT '[]'::jsonb,
    status                TEXT NOT NULL DEFAULT 'draft'
                          CHECK (status IN ('draft', 'candidate', 'qualifying', 'qualified',
                                            'published', 'deprecated', 'withdrawn')),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pack_id, semver)
);

-- --------------------------------------------------------------------------
-- Automation, notification and usage (DOMAIN.md §13)
-- --------------------------------------------------------------------------

CREATE TABLE budgets (
    id                    TEXT PRIMARY KEY CHECK (id LIKE 'bdg\_%'),
    tenant_id             TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id          TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    scope                 TEXT NOT NULL CHECK (scope IN ('tenant', 'workspace', 'run', 'agent_thread')),
    scope_ref             TEXT,
    limits                JSONB NOT NULL DEFAULT '{}'::jsonb,
    consumed              JSONB NOT NULL DEFAULT '{}'::jsonb,
    parent_budget_id      TEXT REFERENCES budgets(id),
    status                TEXT NOT NULL DEFAULT 'active'
                          CHECK (status IN ('active', 'exhausted', 'suspended')),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE routines (
    id                    TEXT PRIMARY KEY CHECK (id LIKE 'rtn\_%'),
    tenant_id             TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id          TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    owner_user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    teammate_id           TEXT,
    trigger               JSONB NOT NULL,
    objective_template    JSONB NOT NULL,
    budget                JSONB NOT NULL DEFAULT '{}'::jsonb,
    notification_prefs    JSONB NOT NULL DEFAULT '{}'::jsonb,
    absence_policy        TEXT NOT NULL DEFAULT 'skip'
                          CHECK (absence_policy IN ('skip', 'queue', 'catch_up_once')),
    status                TEXT NOT NULL DEFAULT 'active'
                          CHECK (status IN ('active', 'paused', 'deleted')),
    last_fired_at         TIMESTAMPTZ,
    next_due_at           TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX routines_due_idx ON routines (next_due_at) WHERE status = 'active';

CREATE TABLE notifications (
    id                 TEXT PRIMARY KEY CHECK (id LIKE 'ntf\_%'),
    tenant_id          TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id       TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id            TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind               TEXT NOT NULL CHECK (kind IN ('needs_approval', 'needs_answer', 'blocked',
                                                     'completed', 'failed', 'mention', 'attention', 'system')),
    ref                JSONB NOT NULL DEFAULT '{}'::jsonb,
    channels_delivered JSONB NOT NULL DEFAULT '[]'::jsonb,
    read_at            TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX notifications_user_idx ON notifications (user_id, read_at, created_at DESC);

CREATE TABLE notification_preferences (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'ntp\_%'),
    tenant_id    TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    channels     JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, user_id, kind)
);

CREATE TABLE usage_records (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'use\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id     TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    scope_refs       JSONB NOT NULL DEFAULT '{}'::jsonb,
    meter            TEXT NOT NULL CHECK (meter IN ('model_input_tokens', 'model_output_tokens',
                                                    'model_cache_tokens', 'model_cost', 'machine_seconds',
                                                    'storage_bytes', 'connector_calls', 'browser_seconds')),
    quantity         DOUBLE PRECISION NOT NULL,
    unit             TEXT NOT NULL,
    cost_minor_units BIGINT,
    source_event_id  TEXT NOT NULL,
    occurred_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, source_event_id, meter)
);
CREATE INDEX usage_records_tenant_time_idx ON usage_records (tenant_id, occurred_at DESC);

CREATE TABLE webhook_subscriptions (
    id               TEXT PRIMARY KEY CHECK (id LIKE 'whk\_%'),
    tenant_id        TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id     TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    url              TEXT NOT NULL,
    secret_handle_id TEXT REFERENCES secret_handles(id),
    event_types      JSONB NOT NULL DEFAULT '[]'::jsonb,
    status           TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'failing')),
    delivery         JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Append-only, hash-chained per tenant (DOMAIN.md §16).
CREATE TABLE audit_entries (
    id             TEXT PRIMARY KEY CHECK (id LIKE 'aud\_%'),
    tenant_id      TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id   TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    actor          JSONB NOT NULL,
    action         TEXT NOT NULL,
    target_ref     TEXT,
    decision       TEXT,
    reason         TEXT,
    correlation_id TEXT,
    occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    prev_hash      TEXT,
    hash           TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_entries_tenant_time_idx ON audit_entries (tenant_id, occurred_at DESC);

-- --------------------------------------------------------------------------
-- Teammates: persistent agent definitions (DOMAIN.md §0 glossary, §5.1)
-- --------------------------------------------------------------------------

CREATE TABLE teammates (
    id                     TEXT PRIMARY KEY CHECK (id LIKE 'agt\_%'),
    tenant_id              TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    workspace_id           TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name                   TEXT NOT NULL,
    persona                TEXT,
    standing_instructions  TEXT,
    default_capability_needs JSONB NOT NULL DEFAULT '[]'::jsonb,
    memory_scope           TEXT NOT NULL DEFAULT 'workspace'
                           CHECK (memory_scope IN ('user', 'workspace', 'teammate')),
    status                 TEXT NOT NULL DEFAULT 'active'
                           CHECK (status IN ('active', 'archived')),
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX teammates_workspace_idx ON teammates (workspace_id, status);

-- --------------------------------------------------------------------------
-- Rebuildable derived structures: NEVER authoritative (DOSSIER.md §9)
-- --------------------------------------------------------------------------

CREATE TABLE derived.embeddings (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'emb\_%'),
    tenant_id    TEXT NOT NULL,
    workspace_id TEXT,
    source_kind  TEXT NOT NULL,
    source_ref   TEXT NOT NULL,
    model_id     TEXT NOT NULL,
    -- Dimension is pinned to the configured embedding model (config/models.yaml);
    -- ANN indexes are dimension-specific. INT-011 owns changing it via migration.
    dimensions   INTEGER NOT NULL DEFAULT 1536 CHECK (dimensions = 1536),
    embedding    vector(1536) NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, source_kind, source_ref, model_id)
);
CREATE INDEX embeddings_hnsw_idx ON derived.embeddings USING hnsw (embedding vector_cosine_ops);

CREATE TABLE derived.index_epochs (
    id           TEXT PRIMARY KEY CHECK (id LIKE 'idx\_%'),
    tenant_id    TEXT NOT NULL,
    index_kind   TEXT NOT NULL,
    epoch        BIGINT NOT NULL,
    rebuilt_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    rebuildable  BOOLEAN NOT NULL DEFAULT TRUE,
    UNIQUE (tenant_id, index_kind, epoch)
);

-- --------------------------------------------------------------------------
-- Row-level security and least privilege
-- --------------------------------------------------------------------------

-- Tables carrying tenant_id. RLS is FORCED so table owners are subject to it too.
DO $$
DECLARE
    t TEXT;
    tenant_tables TEXT[] := ARRAY[
        'tenant_memberships', 'workspaces', 'workspace_memberships', 'service_principals',
        'secret_handles', 'connector_instances', 'threads', 'thread_participants', 'messages',
        'questions', 'work_nodes', 'work_edges', 'agent_threads', 'agent_graph_edges', 'runs',
        'turns', 'steps', 'attempts', 'protocol_states', 'compaction_epochs', 'checkpoints',
        'capability_projections', 'policies', 'user_rules', 'policy_decisions', 'approval_requests',
        'approval_receipts', 'effect_records', 'tool_calls', 'commands', 'runtime_events',
        'event_outbox', 'event_cursors', 'execution_targets', 'leases', 'browser_sessions',
        'terminal_sessions', 'artifacts', 'artifact_versions', 'artifact_grants', 'evidence',
        'evidence_bundles', 'context_projections', 'model_routes', 'model_calls', 'knowledge_entries',
        'memory_entries', 'skills', 'skill_versions', 'capability_packs', 'capability_pack_versions',
        'budgets', 'routines', 'notifications', 'notification_preferences', 'usage_records',
        'webhook_subscriptions', 'audit_entries', 'teammates'
    ];
    -- Derived structures carry tenant_id too: cross-tenant retrieval must be impossible
    -- even for rebuildable indexes.
    derived_tables TEXT[] := ARRAY['embeddings', 'index_epochs'];
BEGIN
    FOREACH t IN ARRAY tenant_tables LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        -- Fail closed: with no tenant context the setting is NULL and no row matches.
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I USING (tenant_id = current_setting(''quansio.tenant_id'', true)) '
            'WITH CHECK (tenant_id = current_setting(''quansio.tenant_id'', true))', t);
    END LOOP;
    FOREACH t IN ARRAY derived_tables LOOP
        EXECUTE format('ALTER TABLE derived.%I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE derived.%I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON derived.%I USING (tenant_id = current_setting(''quansio.tenant_id'', true)) '
            'WITH CHECK (tenant_id = current_setting(''quansio.tenant_id'', true))', t);
    END LOOP;
END $$;

-- Tenants themselves are reachable only for the active tenant.
ALTER TABLE tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenants FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_self ON tenants
    USING (id = current_setting('quansio.tenant_id', true))
    WITH CHECK (id = current_setting('quansio.tenant_id', true));

-- Users are global identities; tenant visibility is mediated by tenant_memberships.
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE users FORCE ROW LEVEL SECURITY;
CREATE POLICY user_self_or_member ON users
    USING (
        id = current_setting('quansio.user_id', true)
        OR EXISTS (
            SELECT 1 FROM tenant_memberships m
            WHERE m.user_id = users.id
              AND m.tenant_id = current_setting('quansio.tenant_id', true)
        )
    )
    WITH CHECK (id = current_setting('quansio.user_id', true));

-- Application role: never a superuser, never the table owner, so RLS applies.
-- Idempotent under concurrent migration from several databases in one cluster.
DO $$
BEGIN
    CREATE ROLE quansio_app NOLOGIN;
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

GRANT USAGE ON SCHEMA public, derived TO quansio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO quansio_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA derived TO quansio_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO quansio_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA derived GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO quansio_app;

-- --------------------------------------------------------------------------
-- updated_at maintenance (authoritative, not application-driven)
-- --------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION set_updated_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END $$ LANGUAGE plpgsql;

DO $$
DECLARE
    t TEXT;
    stamped TEXT[] := ARRAY[
        'tenants', 'users', 'tenant_memberships', 'workspaces', 'workspace_memberships',
        'service_principals', 'secret_handles', 'connector_instances', 'teammates', 'threads',
        'thread_participants', 'messages', 'questions', 'work_nodes', 'work_edges', 'agent_threads',
        'agent_graph_edges', 'runs', 'turns', 'steps', 'attempts', 'protocol_states',
        'compaction_epochs', 'checkpoints', 'capability_projections', 'policies', 'user_rules',
        'policy_decisions', 'approval_requests', 'approval_receipts', 'effect_records', 'tool_calls',
        'commands', 'event_outbox', 'event_cursors', 'execution_targets', 'leases', 'browser_sessions',
        'terminal_sessions', 'artifacts', 'artifact_versions', 'artifact_grants', 'evidence',
        'evidence_bundles', 'context_projections', 'model_routes', 'model_calls', 'knowledge_entries',
        'memory_entries', 'skills', 'skill_versions', 'capability_packs', 'capability_pack_versions',
        'budgets', 'routines', 'notifications', 'notification_preferences', 'usage_records',
        'webhook_subscriptions', 'audit_entries'
    ];
BEGIN
    FOREACH t IN ARRAY stamped LOOP
        EXECUTE format(
            'CREATE TRIGGER %I_set_updated_at BEFORE UPDATE ON %I FOR EACH ROW EXECUTE FUNCTION set_updated_at()',
            t, t);
    END LOOP;
END $$;
