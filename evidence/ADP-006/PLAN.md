# ADP-006 implementation checklist

Order is the task's own; each step is a commit on `task/ADP-006-output-generators` with its tests.

- [x] Authority text first: DOMAIN.md §0, §7.5, §10.1, §10.4 A-G, §12 rule 8, §15; `config/outputs.yaml` and its schema;
      13 tool declarations; three error codes; migration 0027 (`quality_record`).
- [x] (i) Slides: templates and layouts in Python, OOXML writer, calls; authority-side parse, render sandbox in qworkerd
      (fresh browser context, request interception, no script, measurement program), per-page checks, deck states,
      completion gate, `deck.render` / `deck.export` (PPTX verified by an independent reader, PDF) / `deck.deliver`;
      goldens; forgery tests (an upload cannot pass as the authority's pages).
- [x] (ii) Markdown to PDF through the same sandbox; diagram IR, Mermaid subset, layout, SVG safety and IR verification.
- [x] (iii) Video: timeline Artifact (version number is the revision), typed atomic edit operations (batches, expected
      revision, dry run, replay), render plan as typed data with goldens, `MediaSupervisor` (attach by job id, cancel kills
      the process group, timeout, restart recovery), `video.edit` / `video.render` / `video.inspect`.
- [x] (iv) Speech-to-text: gateway route from the catalog, data-policy guard before a byte is built, multipart wire,
      submit-and-poll jobs idempotent by `request_id`, `audio.transcribe` host with its own validation, transcript
      labelled `UNTRUSTED_EXTERNAL`, live suite that is `BLOCKED_EXTERNAL` without credentials.
- [x] (v) Web-dev preview: scopes (token, ttl, maximum lifetime, renewal, close), per-request authority through a port
      with a database implementation, loopback static server (headers, host check, traversal and link refusal), staged
      suggestions as untrusted data, four tools; an HTTP serving test over real sockets. Out of scope and said so: an
      in-iframe edit runtime, a proxy to a running dev server.
- [x] Desktop: quality records, deck page states, transcript and timeline summaries as projections (text nodes only).
- [x] Mutation checks of the guards (each mutant compiles; controls pass); full workspace, Python, repo and desktop runs;
      static gates; evidence bundle; progress; finalize script.
