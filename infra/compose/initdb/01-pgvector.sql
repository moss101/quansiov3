-- Quansio V8.1 dev database bootstrap (GOV-007).
--
-- Runs once on first container start via /docker-entrypoint-initdb.d. The
-- `derived` schema holds rebuildable projections (vector index, DOSSIER.md §9);
-- authoritative data stays in the default schema owned by Rust migrations.
CREATE EXTENSION IF NOT EXISTS vector;
CREATE SCHEMA IF NOT EXISTS derived;
