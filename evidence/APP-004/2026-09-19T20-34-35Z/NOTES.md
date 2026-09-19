# APP-004 — Chat, objective and thread experience

## What was built (task branch `task/APP-004-conversation-surface`)

The 2026-09-15 record was a placeholder (docs/review/2026-09-18-m5-placeholder-audit.md):
a 103-line composer stub and a 7-line `api/threads.rs` facade. The conversation surface now
runs end to end over the runtime, with every identity the server's and no second store.

### Server (`crates/server`)

- **Live frames (§9.3).** `LiveFramePort` on `RuntimeEngine`; the engine publishes
  `progress {stage: model_call}` when a model step dispatches and `model.delta {text, final}`
  when the proposal lands, on `run:<id>` and `workspace:<ws>`. `api/live.rs` is the
  in-process per-tenant hub; `/v1/stream` forwards frames on subscribed channels alongside
  durable events. Frames are transient and never move the cursor.
- **`api/threads/`** (the task's `paths`): `GET /v1/threads?workspace_id`,
  `GET /v1/runs/{id}/timeline` (the per-turn inspector: turns → the model call and the route
  that answered it, the context projection's sources with trust labels, tool calls with
  their effect class/status, the wait step's question with its answer, and the trigger
  message with attachments), `GET /v1/questions?run_id|thread_id` (thread scope joins
  through `runs.trigger_ref` because the runtime does not know conversation threads),
  `GET /v1/artifacts`, `GET /v1/artifacts/{id}/versions`.
- **Commands (§14, all catalogued):** `AnswerQuestion` (RUN-011 answer → the released run's
  next turn runs in the same request → the reply is posted), `CancelTurn` (cancel the run +
  withdraw its open questions with `question.cancelled`), `PostMessage.attachments`
  (validated `art_` ids of the workspace), `CreateArtifact` (bounded base64 upload →
  Artifact + version through CORE-007's store; `TARGET_UNAVAILABLE` without an object
  store; replay by `command_id`).
- **Agent replies are durable messages** (`author_kind = agent`, stamped `run_id`/`turn_id`);
  `Message` and the messages projection carry `author_kind`, `run_id`, `turn_id`,
  `attachments`.
- **Model-call ledger.** Nothing recorded `model_routes`/`model_calls` before, so the
  inspector could not say what answered. `runtime::turn_loop::model_calls::ModelCallLedger`
  is the gateway boundary's record (DOMAIN §11.1); the in-process conformance seam records
  itself honestly as `in-process/conformance-stub` chosen by `seam:conformance`. The API
  ownership gate (`api` writes only `commands`) still holds.
- **Stub scenario.** `[[scenario:question]]` in the trigger message makes the seam ask a
  `single_choice` question and, after the answer, reply `done: you chose <option>`; the
  seam reads the trigger through the runtime's `ModelCallRequest.tenant_id`.
- **Composition:** `QUANSIO_OBJECT_STORE=1` wires the S3/MinIO store from
  `QUANSIO_TEST_MINIO_*`; without it uploads fail closed.
- **Fix found on the way:** `commands.rs` mapped runtime error codes by spellings the runtime
  never used (`RUNTIME_STATE_CONFLICT`, …), so every runtime refusal was a 500 `INTERNAL`.
  Mapped by `RuntimeError::code` (`CONFLICT_STATE` → 409, `NOT_FOUND`, `VALIDATION_SCHEMA`,
  `CAPABILITY_DENIED`).

### Desktop (`apps/desktop/src/conversation/`)

- `state.ts`: the pure reducer — messages, run status, the streamed preview for the
  current turn (dropped once the durable reply lands), step/tool cards, questions, the
  composer; frames for runs the thread does not own are ignored; events mark projections
  stale, the renderer re-reads them.
- `composer.ts`: the commands the page sends; `inspector.ts`: the timeline decoder.
- Bridge channels `threads.list`, `threads.messages`, `questions.list`, `runs.timeline`,
  `messages.post`, `questions.answer`, `turns.cancel`, `artifacts.upload` (size-bounded
  before the send; the renderer reads a file it was handed, main never opens a path).
- `renderer/Conversation.tsx`: thread list, messages, streaming bubble, cards, question
  cards answered in the thread, composer with attachment chips, cancel, per-turn inspector.
- Bundle fix: the CommonJS main derived nothing from `import.meta.url` and would have
  crashed at startup; the bundle now defines it from `__filename`.

## Acceptance

1. **Chat and long objective survive client reload** — `conversation.e2e.test.ts` (real
   server, scratch DB): post → live `model.delta` and `progress` reach the page → the reply
   lands as a durable agent message and replaces the preview → the bridge is torn down and a
   new page rebuilds the same messages from the projections, restoring the thread from the
   durable session; the run's inspector shows `work_node_id` (the objective's WorkGraph
   identity). Reducer reload test in `conversation.test.ts`.
2. **User can inspect which model/context/tools were used** — the timeline shows
   `in-process/conformance-stub` (`seam:conformance`), the context sources with trust
   labels, `fs.read` with its effect status; asserted in both the Rust and the desktop E2E.
3. **Attachments and answered questions survive reload and appear in the run timeline** —
   a real 3000-byte binary (PNG signature, every byte value) uploads through MinIO, is
   attached, appears on the message projection, in the timeline's trigger message, and in
   `/v1/artifacts` with the same digest; the question is asked, answered `beta` in the
   thread, the reply `done: you chose beta` follows, and the timeline's wait step carries the
   answered question. Cancel: the run stops (`CANCELLED`), the question is withdrawn, a late
   answer is `CONFLICT_STATE`, and the page loses its cancel.

## Tests (task list)

| test | where |
|---|---|
| streaming UI test | `apps/desktop/tests/conversation.test.ts` "streaming UI"; E2E live frames; `tests/conversation_surface.rs::live_frames_reach_a_workspace_subscriber_while_the_turn_runs` |
| reload/resume test | `conversation.e2e.test.ts` case 1; reducer "reload" |
| cancel test | `conversation.e2e.test.ts` case 3; `conversation_surface.rs::cancel_turn_stops_a_parked_run_and_withdraws_its_question` |
| attachment/question E2E | `conversation.e2e.test.ts` cases 2 and 4; `conversation_surface.rs::{a_question_parks…, an_upload_becomes_an_attachment…, without_an_object_store_uploads_fail_closed}` |

## Limits, stated

- The in-process seam produces the whole assistant text in one `model.delta {final: true}`;
  token-level streaming arrives with the gateway seam (INT-002/INT-003 wiring into the Rust
  runtime is not this task). The reducer and the page handle both.
- `questions.thread_id` stays null from the runtime (it does not know conversation
  threads); the projection resolves the thread through the trigger message.
- The renderer's DOM is exercised by build/typecheck; behaviour is tested through the
  reducer and the bridge, which is what the page is made of.
