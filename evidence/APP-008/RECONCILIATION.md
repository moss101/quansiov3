# APP-008 reconciliation (2026-09-19, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `apps/desktop/src/artifacts/workspace.ts` (88 lines
of type aliases and a `previewMode` table) with no browser, no server reads, no search
(docs/review/2026-09-18-m5-placeholder-audit.md).

- **Canonical owners:** `apps/desktop/src/artifacts/` (the surface, the task's `paths`) over
  CORE-007's `ArtifactStore`/`EvidenceStore` (`crates/server/src/artifacts/`) and INT-004's
  `SearchIndex` (`crates/indexer/`). The HTTP projections live with the other conversation
  projections in `crates/server/src/api/` (`threads/` for artifacts, a new `search.rs` for
  `/v1/search`). No second store: artifacts, versions, evidence and knowledge are the
  canonical tables; the search index is INT-004's derived, rebuildable projection.
- **Existing code, coverage PARTIAL, kept:** `GET /v1/artifacts` and
  `/v1/artifacts/{id}/versions` (APP-004), `CreateArtifact`, `ArtifactStore::fetch_bytes`
  (digest-verified read), `EvidenceStore`, `SearchIndex` + `PostgresArtifactSource` (typed
  `SearchProgram`, exact/lexical channels, provenance, budgets, per-tenant Tantivy files).
- **Gaps:** no way to read a version's bytes over the API; the versions projection has no
  produced-by/evidence links; `/v1/search?q=` (catalogued in DOMAIN.md §14) is not served;
  the desktop has no artifact browser, preview, diff or search.
- **Persistent state touched:** none new; the search index root
  (`QUANSIO_SEARCH_INDEX_ROOT`) holds derived files only.
- **Contracts (DOMAIN.md §14):** `GET /v1/search?q=` (typed SearchProgram: `q`, `workspace_id`,
  `channels ⊆ {artifacts, threads, knowledge, evidence}`, `max_results`, no free-form
  predicates), `GET /v1/artifacts` (+ `kind`, `run_id` filters), `/v1/artifacts/{id}/versions`
  (+ `produced_by`, `evidence_ids`, `origin`), and the version's bytes at
  `/v1/artifacts/{id}/versions/{vid}/content` — added to the §14 read-projection row in
  the same commit (additive; contracts regenerated). `GET /v1/evidence/{id}` (catalogued,
  unserved) is served for the evidence links.
- **External effects:** none new (object reads go through CORE-007's store).
- **Capabilities/approvals:** unchanged; reads are grant-checked by CORE-007 (`fetch_bytes`
  needs a read grant), and the API's list projections stay tenant/workspace scoped.
- **Migrations:** none.
- **Real boundaries:** none declared. The E2E runs the real binary on a scratch database
  with the dev MinIO and a temporary index root.
- **Security invariant (acceptance 2):** the renderer never loads artifact bytes as a
  document. Preview is text (HTML/SVG/JS/XHTML shown as text, never rendered), a
  same-process `<img>` from a `blob:` URL for raster images only, or download-only; "open"
  hands the saved file to the OS through main (`shell.openPath`) only after the user asked,
  and never for active content types. The CSP (APP-003) already forbids inline script and
  foreign origins.

## Plan

1. Server: `GET /v1/artifacts/{id}/versions/{vid}/content` (bytes, `nosniff`, attachment
   disposition), versions projection with `produced_by` and `evidence_ids`, list filters,
   `GET /v1/evidence/{id}`, `GET /v1/search` executing a typed program over four channels
   (artifacts: INT-004 index for content when `QUANSIO_SEARCH_INDEX_ROOT` is set, plus
   titles from Postgres; threads: message text; knowledge: `knowledge_entries`; evidence:
   `evidence.inline_summary`). The index is rebuilt for the scope when its corpus
   fingerprint changed and updated incrementally on `CreateArtifact`.
2. Desktop `artifacts/`: `workspace.ts` (browser state: list, filters by kind, selected
   artifact, version history, diff between text versions, evidence/source links),
   `preview.ts` (the safe preview decision + text/image/download modes, size bounds),
   `search.ts` (typed query builder + result decoding), bridge channels (`artifacts.list`,
   `artifacts.versions`, `artifacts.content`, `artifacts.save`, `artifacts.open`,
   `evidence.get`, `search.query`), renderer `Artifacts.tsx`.
3. Tests: artifact persistence E2E (upload → run closes/cancels → artifact still listed and
   readable, version history intact, search finds it), malicious preview test (HTML/SVG/JS
   bytes never rendered; open refused for active types; `<script>` in the text preview is
   inert), version history test (two versions, diff, produced-by/evidence links).
