# APP-001 — a command is answered when it is accepted; the turn runs after it

Found by QA-009 against DOSSIER §21.2 ("API mutating command accepted ≤ 500 ms p95") and DOMAIN.md §9
("all commands return `{command_id, accepted_at, result | error}`"):

1. `PostMessage`, `AnswerQuestion` and the approval decisions (`ApproveEffect`, `DenyEffect`) ran the
   run's next turn inside the request and answered only when it had finished. With the in-server stub
   model QA-009 measured `PostMessage` at p50 759 ms / p95 1,036 ms under eight clients; with a real
   model a command would take as long as the model — seconds.
2. No command response carried `accepted_at`, although `schemas/openapi/public-api-v1.yaml` requires
   it on every command response (SDKs typed from it expect it).

Fix:
- The served composition (`composition.rs`) builds the API with `ApiState::with_background_turns`:
  those commands answer once their own state is committed (message posted and run started; answer
  recorded; decision and receipt recorded) and the turn runs after it
  (`commands::continue_run`). Its progress, reply, and any question or approval it raises reach
  clients as events and live frames — the desktop already renders them that way — and a turn's failure
  is the run's own. A composition without the flag (the router-level tests) keeps waiting for the
  turn and reports its outcome, so those tests keep asserting turn internals directly.
- `CommandResponse.accepted_at` is the command record's `created_at`; a replay reports the original.
- Clients of the served binary that read a turn's result from the command now wait for the turn: the
  QA-007 campaign (`settle`), the desktop roster, collaboration, work and trust E2E
  (`apps/desktop/tests/settle.ts`), and the web approval E2E.

Tests: `served_a_message_is_accepted_before_its_turn_and_the_reply_follows` (a model that takes 800 ms:
the command answers in well under 600 ms with `accepted_at` and a running run, the reply follows, a
resend reports the same `accepted_at`); the whole `quansio-server` suite (`server-tests.log`); the
desktop and web suites including their E2E against this build (`desktop-tests.log`, `web-tests.log`);
the security campaign and the Python SDK compatibility test (`security-sdk-tests.log`); the TypeScript
SDK suite (`sdk-ts-tests.log`); fmt and clippy (`lint.log`). QA-009 measures command acceptance on the
release build.

Mutation (`mutation.log`): a command that always waits for its turn fails the acceptance test. The first
full run of the suites (not kept) found the E2E suites that read a turn's results straight after the
command, and the QA-007 serving-role check counting a backend still authenticating; they were changed
to wait for the turn and to ignore that backend, and every suite then passed on this branch. The
desktop and web E2E ran against a debug build of 8898b1f; later commits change tests and one clone.
