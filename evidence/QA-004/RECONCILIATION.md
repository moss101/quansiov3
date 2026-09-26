# QA-004 reconciliation — qualify model, context, search, memory and skills

**Canonical owner.** `tests/evaluation/` (the campaign, its pinned sets and its frozen baseline).
The campaign drives shipped owners and never re-implements them: the evaluation harness
(`python/intelligence/evaluation`, INT-010), the model gateway (INT-002/003), the context plane
(INT-005), the derived index and memory fabric (INT-007/011), skills (INT-009, CAP-006/008) and the
Tool Registry's argument check (`crates/tools`, RUN-011).

## Existing code and coverage — `PARTIAL`

`tests/evaluation/test_qa004_campaign.py` (2026-09-15, reset by the placeholder audit) passes on
`main`, but it does not qualify what the task names:

| Build item | State on `main` |
|---|---|
| route | `measure_route_quality` over the **conformance stub catalog**, not `config/models.yaml` |
| retrieval | recall over a 3-document corpus written into the test (self-selected); `retrieval.v1.json` is pinned but read by nothing; embedder is a hashed bag of words |
| grounding | not measured — the gate test *asserts* it is unmeasured |
| context budget | not evaluated |
| memory isolation | `measure_retrieval` deletes an *artifact* source from the embedding index and discards its tenant arguments; memory (`MemoryFabric`, `forget_memory`) is never exercised; the SQL leg runs as superuser, so row-level security is bypassed |
| skill regression | resolution dataset + WikiSkill campaign; the shipped packs' suites and CAP-008's regression gate are not run |
| tool proposals | not measured — the Tool Registry check has no Python-callable seam |
| cost/latency | not recorded (`CostLatency(wall_time_ms=0)`) |
| protected regressions | computed against the run itself (`protected_regressions(x, x)` is always 0) |

The INT-010 port datasets cannot gate a real port as they stand: `grounding.v1` and
`proposal_validity.v1` are *labelled calibration cases* (2 of 3 pinned unsupported, 2 of 3 pinned
invalid), so a correct verifier reports 0.667 unsupported / 0.333 valid. The gated rates must be taken
over a population the system produced; the labelled cases calibrate the judge.

## Persistent state touched

None in product stores. The DB legs create and drop scratch databases (`scripts/dev/seed_test_database.py`)
and run the product's own writers there as `quansio_app`.

## Contracts / events

No contract or event change. One additive harness change: `context_budget` joins the dataset kinds
(`python/intelligence/evaluation/datasets.py`), because the task names context budget as an evaluation
family and the harness refuses kinds it does not know.

## Real boundaries and environment

| Boundary | Variables | Legs |
|---|---|---|
| Live model (OpenAI-compatible wire) | `QUANSIO_TEST_OPENAI_COMPATIBLE_{BASE_URL,API_KEY,MODEL_ID}` | research answers (grounding population), tool proposals, context accepted by a provider, cost/latency |
| Live model (Anthropic wire) — the judge | `QUANSIO_TEST_ANTHROPIC_API_KEY` (+ optional `_BASE_URL`, `_MODEL_ID`) | grounding judge, calibrated against the labelled set before it is trusted |
| Live embedding route (`config/models.yaml` `openai-text-embedding-3-small`) | `QUANSIO_TEST_OPENAI_API_KEY` | retrieval recall@10 on the pinned corpus |
| PostgreSQL + pgvector | `QUANSIO_TEST_POSTGRES_URL` | memory isolation adversarial set under RLS, retrieval on the real derived table, index query latency |

An absent variable skips its leg with `BLOCKED_EXTERNAL` naming it; the campaign gate then reports the
metric unmeasured (fails closed) — never a simulated value. Z.ai serves no embedding model (probed
2026-09-25: `embedding-3`/`embedding-2` → `1211 Unknown Model`), so recall with a real embedder needs
`QUANSIO_TEST_OPENAI_API_KEY`.

Both configured live wires serve the same model (`GLM-5.3-Flash`), so the judge is independent in role,
prompt and wire but not in weights. The judge is therefore **calibrated** on the pinned labelled set
before its verdicts count, and the campaign records both model ids.

## Effects, capabilities, migrations, rollback

No external effects (model calls propose only; no tool executes). No migration. Rollback: the campaign
is read-only verification; reverting the branch removes it.

## Dependencies

INT-010 PASS; CAP-006 and CAP-008 BLOCKED_EXTERNAL (implementation complete). QA-004 can therefore
record at most `BLOCKED_EXTERNAL` with `implementation_complete: true` until they pass.

## Plan

- [ ] `crates/tools`: `quansio-tool-check` binary — JSON lines in, the registry's own
      `first_violation` verdict out; Rust test drives the binary.
- [ ] `context_budget` dataset kind (INT-010 harness, additive) + test.
- [ ] Pinned campaign sets under `tests/evaluation/campaign/` (digest-checked `Dataset` files):
      retrieval corpus + queries, memory adversarial set, grounding calibration, research questions,
      proposal calibration (against real `config/tools.yaml` declarations), proposal tasks,
      context-budget cases; `campaign.yaml` (judge calibration floor, live call and cost budgets);
      frozen protected baseline.
- [ ] Harness (`tests/evaluation/qa004_harness.py`): live boundaries from env, tool-check client,
      answer generator through the context plane, calibrated judge, memory adversarial runner under
      `quansio_app`, context evaluator, skill regression over every shipped pack, cost/latency ledger,
      run assembly + evidence writer.
- [ ] Tests: route on the config catalog; context budget; skill regression; proposal calibration;
      memory isolation (RLS); retrieval on pgvector + index latency vs §21.2; live grounding; live tool
      proposals; live recall; the campaign gate (all rows when every boundary is present, protected rows
      always).
- [ ] Negative tests: a leaking store, a broken checker, an uncalibrated judge, a regressed baseline
      each fail the campaign.
- [ ] Mutation checks, CI, evidence, progress.
