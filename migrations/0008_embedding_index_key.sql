-- Derived embedding index keys (INT-011).
--
-- `derived.embeddings` was created with one row per (tenant, source, model). The embedding
-- pipeline indexes a source in chunks, so a row is keyed by its provenance as well: the
-- source snapshot the text belongs to, the digest of the chunk text and the chunk's ordinal.
-- That key is what lets a rebuild prove it reproduced the same content, lets an unchanged
-- chunk be reused instead of re-embedded, and lets a superseded snapshot or a rewritten
-- chunk be deleted by key rather than by scanning.
--
-- The table is derived and rebuildable (migrations/README.md), so this migration only widens
-- the key: a populated index is rebuilt from the authoritative sources rather than backfilled
-- from rows whose provenance is no longer known. Forward-only; recovery is OPS-005.

ALTER TABLE derived.embeddings
    ADD COLUMN IF NOT EXISTS snapshot       TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS content_digest TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS chunk_index    INTEGER NOT NULL DEFAULT 0;

ALTER TABLE derived.embeddings
    DROP CONSTRAINT IF EXISTS embeddings_tenant_id_source_kind_source_ref_model_id_key;

ALTER TABLE derived.embeddings
    ADD CONSTRAINT embeddings_source_chunk_key
    UNIQUE (tenant_id, source_kind, source_ref, model_id, snapshot, content_digest, chunk_index);

ALTER TABLE derived.embeddings
    DROP CONSTRAINT IF EXISTS embeddings_chunk_index_check;

ALTER TABLE derived.embeddings
    ADD CONSTRAINT embeddings_chunk_index_check CHECK (chunk_index >= 0);
