# Artifact-edit skill

Artifacts are versioned source files (DOMAIN.md §10.1), not chat attachments.

1. Choose a kind and write its source: `document` (markdown starting `# Title`, with a body),
   `spreadsheet` (CSV, unique headers, rectangular rows), `presentation` (slides JSON, every
   slide with a body), `code` (files JSON, relative paths), `data` (rows JSON), or media bytes.
2. Propose `artifact.create {role, kind, title, content | content_base64, content_digest,
   media_type}` for the first version, `artifact.update {artifact_id, content |
   content_base64, content_digest}` for each edit. `content_digest` is the lowercase hex
   SHA-256 of the exact bytes. The server refuses a source that fails its check or its digest,
   and creates no version; fix the source and propose again.
3. Each version is immutable; an edit becomes the next version, its parent the one it
   replaced. Preview is digest-and-excerpt evidence and never executes HTML/JS/SVG.
4. The work is done when its contract's `artifact_valid` check passes on the current version —
   a malformed deliverable (even one a person uploaded) blocks completion.

`fixtures/corpus.yaml` lists valid and malformed sources with the verdict both the server and
the intelligence plane must reach.
