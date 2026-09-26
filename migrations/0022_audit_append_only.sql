-- Audit is append-only in the database, not only by convention (QA-007 finding F-3; DOSSIER.md
-- §16 "Append-only hash-chained audit"). The chain in `hash`/`prev_hash` is an unkeyed digest: it
-- detects an edited entry only when the later hashes were left alone, and a writer holding the
-- application's credentials could rewrite an entry and recompute every later hash. So the
-- application role loses UPDATE, DELETE and TRUNCATE on `audit_entries`, and a trigger refuses
-- them for every role that can still reach the table (the owner included) — removing it needs
-- DDL on the table, which the serving role never holds (QUANSIO_MIGRATION_DATABASE_URL).
-- Nothing in the product edits or deletes an audit entry. Forward-only; additive.

REVOKE UPDATE, DELETE, TRUNCATE ON audit_entries FROM quansio_app;

CREATE OR REPLACE FUNCTION audit_entries_append_only() RETURNS TRIGGER
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_entries is append-only: % refused', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END $$;

CREATE TRIGGER audit_entries_append_only
    BEFORE UPDATE OR DELETE ON audit_entries
    FOR EACH ROW EXECUTE FUNCTION audit_entries_append_only();

CREATE TRIGGER audit_entries_no_truncate
    BEFORE TRUNCATE ON audit_entries
    FOR EACH STATEMENT EXECUTE FUNCTION audit_entries_append_only();
