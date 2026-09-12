-- RUN-011: a tool call records the Tool declaration version it was planned against, so a
-- resumed call re-plans against the same version instead of silently picking up a newer
-- declaration (DOMAIN.md §7.5).
ALTER TABLE tool_calls
    ADD COLUMN IF NOT EXISTS declaration_version INTEGER NOT NULL DEFAULT 1;
