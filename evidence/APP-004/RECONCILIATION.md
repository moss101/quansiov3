# APP-004 reconciliation (2026-09-19, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `apps/desktop/src/conversation/composer.ts` (103
lines of URL/param helpers) and a 7-line `crates/server/src/api/threads.rs` facade
(docs/review/2026-09-18-m5-placeholder-audit.md).

- **Canonical owners:** `crates/server/src/api/threads/` (the HTTP projection + command surface
  of the conversation owner `control::conversation` and the runtime) and
  `apps/desktop/src/conversation/` (the surface). Matches `paths`. No second store: threads,
  messages, questions, turns, steps, tool calls, model calls, context projections and
  artifacts are the canonical tables, written by their owners.
- **Existing code, coverage PARTIAL, kept:** `ConversationStore` (threads/messages),
  `PostMessage`/`CreateThread`/`CancelRun`/`PauseRun`/`ResumeRun`, the run/thread/messages/
  effects projections, `QuestionService` (RUN-011: create/answer/expire, parks and resumes the
  run), `ArtifactStore` (CORE-007: create/add_version over the S3 object store), the
  `ConformanceStubModel` seam.
- **Persistent state touched:** none new. `messages.attachments` (existing JSONB) carries
  artifact ids; artifacts get `origin {kind: message}` on upload-from-composer.
- **Contracts (DOMAIN.md §14, all catalogued):** commands `AnswerQuestion`, `CancelTurn`,
  `PostMessage` (with `attachments`), `CreateThread`; read projections `/v1/threads`,
  `/v1/runs/{id}/timeline`, `/v1/questions`, `/v1/artifacts`, `/v1/artifacts/{id}/versions`.
  Upload is the catalogued command `CreateArtifact` ("upload init/complete", §14) taking a
  bounded base64 body in one step; no new route, and idempotent by `command_id` like every
  command (the plan's `POST /v1/artifacts` was dropped for it). Live frames `model.delta` and
  `progress` (§9.3) are published by the runtime through a `LiveFramePort` and forwarded by
  `/v1/stream` from an in-process hub; they are transient and never persisted.
- **External effects:** none new (object writes go through CORE-007's store).
- **Capabilities/approvals:** unchanged; every mutation is a §14 command.
- **Migrations:** none.
- **Real boundaries:** none declared. The E2Es run the real binary on a scratch database with
  the dev MinIO for attachments.
- **Rollback/recovery:** none beyond the runtime's own (a cancelled turn is a run transition
  the runtime already owns).

## Plan

1. Runtime seam: `LiveFramePort` on `RuntimeEngine` — `progress` at model-call/tool
   dispatch/settle, `model.delta` for assistant text; `LiveHub` in the API forwards to
   `/v1/stream` subscribers by channel.
2. `api/threads/`: thread list, run timeline (turns → model calls with route, context
   projection segments with trust labels, tool calls with effect status, questions,
   attachments), questions projection, artifact upload/list/versions; commands
   `AnswerQuestion` (resume + next turn), `CancelTurn`, `PostMessage` attachments.
3. Stub model scenarios keyed by the trigger message (`[[scenario:question]]`) so the
   question path is exercisable without a live model.
4. Desktop `conversation/`: state reducer over durable events + live frames (messages,
   streaming text, tool/progress cards, questions, run status), composer with attachments,
   in-thread answers, per-turn inspector; bridge channels; renderer.
5. Tests: streaming UI (reducer), reload/resume E2E, cancel E2E, attachment/question E2E.
