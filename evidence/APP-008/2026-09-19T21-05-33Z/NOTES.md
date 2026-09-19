# APP-008 — Artifact, file and document workspace

## What was built (task branch `task/APP-008-artifact-workspace`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
an 88-line `previewMode` table with no browser, no server reads, no search. The workspace
now runs end to end over CORE-007 and INT-004.

### Server (`crates/server`)

- **`api/artifacts.rs`** (the artifact handlers moved out of `threads/`): `CreateArtifact`
  also adds a version to an existing artifact (`artifact_id`, write grant checked by
  CORE-007) and accepts a `kind` override; media types map to the ten artifact kinds
  (document/spreadsheet/presentation/code/data/image/audio/video/archive/other);
  `GET /v1/artifacts` gains `kind`/`run_id` filters, `version_count` and `produced_by`;
  `GET /v1/artifacts/{id}/versions` carries `produced_by` and the `evidence_ids` the
  producing step captured (the source/evidence links); `GET /v1/evidence/{id}` (catalogued,
  previously unserved) is the link target.
- **Bytes:** `GET /v1/artifacts/{id}/versions/{vid}/content` — digest-verified by
  `ArtifactStore::fetch_bytes`, read grant enforced, served as `attachment` with
  `nosniff` and `private, no-store`; active document types (HTML, XHTML, SVG, XML, JS) are
  relabelled `text/plain` on the wire so no client is invited to render them. Added to
  DOMAIN.md §14's read-projection row (additive; contracts regenerated, `--check` clean).
- **`api/search.rs`:** `GET /v1/search?q=&workspace_id=&channels=&max_results=` executes one
  typed program: channels are an enumeration (`artifacts`, `threads`, `knowledge`,
  `evidence`), the budget is bounded (`max_results ≤ 50`), and `q` is a bound parameter
  everywhere (LIKE metacharacters escaped; the injection probe returns nothing). Artifacts
  match by title (canonical table) and by content through INT-004's Tantivy index when
  `QUANSIO_SEARCH_INDEX_ROOT` is set: the service rebuilds a tenant's index from
  `PostgresArtifactSource` + the object store whenever the corpus fingerprint (version
  count, newest version, deletions) changed, and `CreateArtifact` applies its document
  incrementally. Threads match message text, knowledge matches `active`/`verified`
  entries only, evidence matches summaries and kinds. Every hit carries provenance
  `(source_id, locator, snapshot)`; the response names the engine used per channel.

### Desktop (`apps/desktop/src/artifacts/`)

- **`preview.ts`** — the safety decision is structural: HTML/SVG/XHTML/XML/JS are shown as
  text (a `<pre>` text node), raster images are decoded in-process from a `data:` URL (the
  only image source the CSP allows), everything else is download-only; PDFs, programs,
  scripts and unlabelled bytes are never handed to "open".
- **`workspace.ts`** — browser reducer (list with kind filter, selection, version history,
  comparison defaulting to the parent version, preview, evidence links, stale marks on
  `artifact.*` events), projection decoders, LCS line diff (bounded).
- **`search.ts`** — typed program builder (channels enumerated, query bounded, results
  budget bounded) and result decoder.
- **Bridge** — `artifacts.list/versions/content/save/open`, `evidence.get`, `search.query`.
  `content` returns bounded bytes with the preview plan main computed; `save` goes through
  Electron's save dialog; `open` writes under `userData/artifacts` and calls
  `shell.openPath` only for media that is neither active nor executable (checked against
  the *recorded* media type, not the transport label). Without injected file operations
  (tests, a locked-down host) save/open are refused, never faked.
- **Renderer `Artifacts.tsx`** — browser, global search with channel toggles, version
  history with produced-by run links and evidence links, save/open, diff view, preview;
  the shell now switches between the conversation and artifacts surfaces.

## Acceptance

1. **Artifacts remain available after chat/run closure** — Rust
   `artifacts_outlive_the_run_and_keep_their_version_history_with_links` and the desktop
   E2E: a run is created and cancelled; the artifact is listed, its two versions (with the
   replay of identical bytes collapsing to the same version) and evidence link resolve, and
   a reloaded page over the same server lists the same artifacts, history and preview.
2. **Preview cannot execute active content with host privileges** — Rust
   `bytes_are_served_as_an_attachment_never_as_a_document_and_only_to_a_reader` (transport
   never invites rendering; a non-reader gets 403) and the desktop tests: an HTML upload
   with `<script>` and an SVG with `onload` arrive as text plans, the page shows the
   literal markup as a string, `imageDataUrl` refuses SVG, and `artifacts.open` refuses
   both; a PNG previews from a `data:` URL and may be opened.

## Tests (task list)

| test | where |
|---|---|
| artifact persistence E2E | `apps/desktop/tests/artifacts.e2e.test.ts`; `crates/server/tests/artifact_workspace.rs::artifacts_outlive_the_run…` |
| malicious preview test | `apps/desktop/tests/artifacts.test.ts` "malicious preview"; `artifacts.e2e.test.ts` (hostile HTML/SVG); `artifact_workspace.rs::bytes_are_served_as_an_attachment…` |
| version history test | `artifacts.test.ts` "version history" (history, comparison, diff, decoders); E2E history + diff; Rust versions projection with produced-by/evidence links |
| global search (build item 3) | `artifact_workspace.rs::search_is_one_typed_program…`; E2E content + thread hits; `artifacts.test.ts` "global search" |

## Limits, stated

- The content index is rebuilt per tenant when the corpus fingerprint changes (a full
  re-read of that tenant's text objects); fine for the sizes the dev stack holds, and the
  incremental path covers API uploads. Artifacts written by other owners are picked up at
  the next fingerprint change.
- Search over threads/knowledge/evidence is Postgres `ILIKE` over bound parameters (typed,
  case-insensitive, no ranking beyond recency); semantic/graph/memory channels are other
  owners' and are refused by name.
- Document/spreadsheet/slide metadata is the kind and media type CORE-007 records; the
  workspace does not parse Office files.
