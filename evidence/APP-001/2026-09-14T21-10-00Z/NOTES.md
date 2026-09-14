# APP-001 evidence notes

The remaining APP-001 units are implemented: walking skeleton, WebSocket reconnect, per-tenant rate limits, feature flags, OpenAPI/catalog contract, conversation owner, and additional mutating commands that call canonical owners.

## acceptance

**Every mutating endpoint calls the canonical owner rather than writing foreign tables.** The `src/api` scan still fails on any INSERT/UPDATE/DELETE that is not `commands`. CancelRun/PauseRun/ResumeRun call the runtime; CreateThread/PostMessage call `control/conversation`; PostMessage then drives the runtime turn loop.

**Generated clients pass contract tests.** `schemas/openapi/public-api-v1.yaml` is generated from DOMAIN.md. The suite asserts every catalog command has an OpenAPI path; the server serves those paths as `POST /v1/commands/:command`.

**The walking-skeleton smoke test passes in CI with the conformance-stub provider.** `PostMessage` creates a thread and message, starts a run, proposes `fs.read` through the in-process conformance stub, settles through the Effect Ledger, and is readable from `/v1/runs/{id}` and `/v1/effects`. WebSocket `GET /v1/stream` catch-up and reconnect-from-cursor pass.

## status

The task cannot be recorded PASS: dependency INT-002 is `BLOCKED_EXTERNAL`. The walking skeleton uses the conformance stub, which INT-002 itself says never counts as live-provider evidence. Unblock when INT-002 is PASS.
