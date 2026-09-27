# QA-009 evidence — performance, scale and cost controls on the served product

**Tier.** `tests/performance/test_qa009_campaign.py` and `test_qa009_live.py` drive the deployable
`quansio-server` **release** build from outside: real HTTP clients (one keep-alive connection each) and a
real RFC 6455 WebSocket client, against PostgreSQL 17 (dev stack), MinIO and the search index, with the
server serving as a least-privileged `quansio_app` login and sessions from the real magic-link flow. Every
knob is at its shipped default unless a result names the override (the latency phases raise the per-tenant
command quota, a policy, so a burst measures the server rather than the quota; the overload phase keeps
it). The live phase adds the Python intelligence service routed to a live OpenAI-compatible provider
(`QUANSIO_TEST_OPENAI_COMPATIBLE_*`), with the provider called directly, alternately, as the baseline.
`crates/qworkerd/tests/browser.rs` times the browser DOM path on a real Chrome.

The host is a developer Mac shared with other work: its one-minute load average during the final run
was 3.7–6.6 (median 5.6, `host-load.txt`); numbers are an upper bound for a dedicated host. The final
run is `pytest-performance.log` (release build of the branch merged with every fix below, the code of
c3a2179 less its `QUANSIO_TEST_PERF=1` gate, which was added after the run so CI does not start a
half-hour measurement on a busy host).

## Results against DOSSIER §21.2 (provisional SLOs; ratification is the owner's)

| §21.2 metric | Target | Measured | Result |
|---|---|---|---|
| API read | ≤ 200 ms p95 | 13.8 ms p95 over 3,600 reads of 18 read projections, eight clients | pass |
| Mutating command accepted | ≤ 500 ms p95 | 131 ms p95 with eight clients each pausing 0.25 s (thirty commands a second into one workspace, every message starting a turn); 574 ms p95 with no pause at all (a saturating burst, recorded — P-13) | pass (paced) |
| Event delivery lag | ≤ 1 s | 25 ms p95 quiet, 23 ms p95 under a read load (WebSocket, command commit to frame) | pass |
| Model stream first token over the provider | ≤ +300 ms | Quansio's own overhead against the stub provider: +55 ms p50, +80 ms p95; a live GLM-5.3-Flash streams 12/12 replies, 2.9 s p50 later than a bare prompt because the model reasons over Quansio's instructions, skills and tools first | pass (Quansio's overhead) |
| Runtime scheduling latency | ≤ 500 ms p95 | 564 ms p95 at load 5.6 (twenty routines every 5 s, window to run start); 447 ms p95 at load 3.5 (`scheduler-lower-load.log`) | not demonstrated on this host |
| Index query | ≤ 300 ms p95 | 18.5 ms steady; 16.2 ms for the first query after a write | pass |
| Browser action round-trip (DOM) | ≤ 1.5 s | 3.6 ms p95, click then DOM read over CDP on a real Chrome (`browser-dom.log`; the library, not a served turn — P-12) | pass (library) |
| Memory/knowledge deletion visible | ≤ 60 s | 0.027 s (knowledge), 0.020 s (memory) | pass |
| Overload | bounded queues, no unbounded memory | 200-command burst: 120 accepted, 80 `RATE_LIMITED`; 64 readers for 30 s against an 8-connection pool: 0 errors, p95 134 ms, RSS 35.4 → 35.6 MB; 16 stalled stream consumers while 6,000 events were written: RSS 83 → 89.5 MB, a reading consumer's lag 26 ms p95 | pass |
| Soak | steady latency and memory | 10 minutes, eight users: p95 41–62 ms a minute, 0 errors, RSS 27.7 → 36.0 MB (within bound; a longer soak would tell caching from a leak) | pass |
| Usage accuracy | exact | storage: 246,390 bytes written, 246,390 metered after 22,060 events (read 9 ms); model tokens: 5,620 in / 812 out metered, equal to what the provider reported | pass |

## Findings and where they were fixed

Every finding below was fixed on its owner's branch with its own tests, a killed mutant and a served
before/after measurement where one applies (evidence under each owner), then merged before the final
run.

| Id | Finding | Severity | Fix |
|---|---|---|---|
| P-1 | A routine created with the default absence policy (`skip`, the desktop's default) never fired on schedule: every due window was treated as missed. | P1 | CORE-008 (e0c7176) |
| P-2 | Served `/v1/usage` never metered a model token: nothing emitted `model.usage_recorded`. | P1 | RUN-011 (d27affd) |
| P-3 | A model's reply reached a person only whole, after the call: the first-token budget was unreachable. | P1 | RUN-001 (f1f564a) |
| P-4 | `/v1/usage` re-projected the tenant's oldest 20,000 events on every read: nothing newer was metered. | P1 | OPS-004 (67e20d6, clippy correction 01ebe0d) |
| P-5 | Scheduling: a fixed one-second loop, and each claimed fire's whole turn executed inside the pass (p95 3.4 s window to run start). | P2 | CORE-008 (c7f1bfb) |
| P-6 | No cost control was enforced: budgets and quotas had no production caller, no command granted a budget, charges were not atomic. | P1 | RUN-010 / OPS-004 (771bb22) |
| P-7 | A WebSocket subscriber replaying more than 64 events was cut off after 64 frames, however fast it read. | P1 | CORE-003 (5612ae3) |
| P-8 | Artifact changes wrote no `artifact.*` events: storage never metered; artifact triggers, webhooks and streams blind. | P1 | CORE-007 (b2532cb) |
| P-9 | `GET /v1/threads` made one access query per thread (p50 397 ms at ~200 threads). | P2 | APP-009 (5a7a220) |
| P-10 | Every search after an artifact change rebuilt the whole index from object storage (p50 385 ms at 120 notes). | P2 | INT-004 (5715ce3) |
| P-11 | `PostMessage`, `AnswerQuestion` and approval decisions answered only when the turn had finished; no command response carried the `accepted_at` the public API requires. | P2 | APP-001 (b6e7646) |
| P-12 | Browser actions from a served turn reach a conformance stub, and `qworkerd` has no daemon: the DOM path is measured at the library, on a real Chrome, not through the server. | P2 | open — recorded |
| P-13 | Under a saturating single-tenant burst (eight clients, no pause, every message starting a turn) command acceptance reaches p95 ~600 ms on this shared host; one client sees p50 8 ms (CreateThread) / 34 ms (PostMessage). Writers to one tenant also serialize on its event sequence. | P3 | open — recorded; the paced mix is inside the budget |

## Not measurable here

| §21.2 row | Status |
|---|---|
| Cloud microVM warm ≤ 3 s / cold ≤ 30 s | `BLOCKED_EXTERNAL` — no cloud microVM substrate or warm pool exists to start (`crates/machine` has the enum only) |
| macOS local capsule warm start ≤ 15 s | `BLOCKED_EXTERNAL` — `QUANSIO_TEST_MACOS_VM=1` with a signed guest image; `config/guest-images.yaml` marks the image unpublished |
| Server availability 99.9 % monthly | not measurable before a managed deployment runs for a month |
| DR RPO ≤ 5 min, RTO ≤ 60 min | `BLOCKED_EXTERNAL` through OPS-005 (a live PITR drill, `QUANSIO_TEST_PITR=1`) |
| Worker density | nothing to measure: `qworkerd` has no daemon or per-host configuration |

## Pipeline

`ci.log` / `ci-summary.json`: `scripts/ci/ci.sh` PASS at 8e9fd2e (this branch merged with `main`).
Its runs on this loaded host (load average 7–12) exposed three tests that assumed a quiet machine or
a synchronous turn, each fixed on its owner's branch and merged first: the CLI journey read the
parked turn from the command response (APP-013, f7db1eb); a desktop journey read a thread before its
last queued message was answered (APP-001, f6e5d98); a timer due 400 ms out was due before a pool
reconnect finished (CORE-008, 3bf1be8). The toolchains gate took 3,370 s of its 3,600 s budget.
