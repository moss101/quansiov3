# GOV-001 — Repository inventory and V8.1 reconciliation

**Task:** GOV-001 (M0) — Inventory repository and reconcile implementation
**Date:** 2026-09-12
**Branch:** `task/GOV-001-repository-reconciliation`
**Scope:** `docs/review/`, `evidence/GOV-001/`
**Status of this report:** evidence for GOV-001; historical review material, not implementation authority (DOSSIER.md §1).

## 1. Method

Two executable checks back this report, both committed and CI-runnable:

| Check | Command | Result |
|---|---|---|
| Repository inventory | `python3 scripts/ci/inventory.py --json` | see `evidence/GOV-001/<ts>/inventory.json` |
| Duplicate-authority scan | `python3 scripts/ci/inventory.py --scan` | `CLEAN (0 findings)`, exit 0 |
| Tool behaviour tests | `uv run --python 3.12 --with pytest pytest tests/architecture -q` | pass (positive + negative fixtures) |

`scripts/ci/inventory.py` maps every product-code file to its canonical owner using the
`CANONICAL_OWNERS` table derived from DOSSIER.md §5 (ownership) and §17 (layout), and enforces the
locked language boundaries of DOSSIER.md §3 through a named rule set. Findings are per file/line so
a future violation cannot hide behind a summary count.

Scan rules (each has a negative fixture test in `tests/architecture/test_inventory.py`):

| Rule | Detects | Authority |
|---|---|---|
| `no-canonical-owner` | product code with no canonical V8.1 owner | DOSSIER.md §5 |
| `non-rust-authority-write` | Python/TS/JS writing a canonical authority table (`work_graph*`, `runtime_events`, `effect_ledger`, `approval*`, `capability_projection`, …) | DOSSIER.md §3, D-001/D-002 |
| `provider-sdk-outside-gateway` | provider SDK imported outside `python/intelligence/model_gateway/` | D-006 |
| `client-direct-database` | desktop/web app or SDK importing `pg`/`knex`/`prisma`/`sqlite3`/… | DOSSIER.md §4.1 |
| `tool-registry-outside-rust` | tool registration outside the Rust Tool Registry | RUN-011, D-014 |
| `hardcoded-model-id` | model identifier literal outside `config/` | D-018 |
| `new-go-code` | any new `.go` file | D-011 |

## 2. Inventory (factual baseline)

The repository at reconciliation time contains the V8.1 authority set and CI/dev tooling only
(`scripts/validate_v81.py`, `scripts/ci/inventory.py`) plus the tests that drive those tools.

- git: initialised on `main`; authority set committed as `[GOV-001] initialize repository`; GOV-001
  work proceeds on `task/GOV-001-repository-reconciliation`.
- Present components: `scripts/` (tooling), `tests/architecture/` (tool tests), `registries/`,
  `docs/review/`.
- Absent components (every product owner in DOSSIER.md §17): `crates/` (`server`, `core`, `graph`,
  `events`, `capability`, `tools`, `indexer`, `machine`, `qworkerd`, `cli`, `contracts`),
  `python/intelligence/`, `apps/desktop`, `apps/web`, `native/macos`, `native/windows`, `schemas/`,
  `migrations/`, `infra/`, `packs/`, `sdk/`, `config/`.
- Deployables: none (no Dockerfile, compose, Helm chart or Terraform module).
- Databases/schemas: none in-repository (no migration files, no schema sources).
- Build systems: none yet (`Cargo.toml`, `python/pyproject.toml`, `pnpm-workspace.yaml` do not exist).
- CI: none (no workflow files).
- Product surfaces: none.

## 3. Classification against V8.1 canonical owners

**No pre-existing product code was found.** The greenfield rule (AGENTS.md "Existing code and
greenfield", DOSSIER.md OD-012) therefore applies:

- every V8.1 canonical owner is classified `GENUINE_GAP`;
- there is **no legacy authority found** — no legacy Python/FastAPI orchestration, no Go service, no
  parallel store, policy engine, effect path or browser stack exists to migrate or quarantine;
- GOV-002 and GOV-006 are trivially satisfied (no superseded authority to archive; no legacy path to
  delete), recorded in their own evidence with a reference to this report.

| Canonical owner | Classification | Planned path (DOSSIER.md §17) |
|---|---|---|
| `quansio-server` (API/control composition) | `GENUINE_GAP` | `crates/server/` |
| core IDs/generation/idempotency/errors | `GENUINE_GAP` | `crates/core/` |
| graphs + GraphTransaction | `GENUINE_GAP` | `crates/graph/` |
| RuntimeEvent store, outbox, projections | `GENUINE_GAP` | `crates/events/` |
| Capability Projection | `GENUINE_GAP` | `crates/capability/` |
| policy/RBAC/privacy/approvals | `GENUINE_GAP` | `crates/server/policy/` |
| Universal Effect Ledger | `GENUINE_GAP` | `crates/server/effects/` |
| Tool contract + Tool Registry | `GENUINE_GAP` | `crates/tools/` |
| `quansio-indexer` | `GENUINE_GAP` | `crates/indexer/` |
| machine control / worker gateway / egress / secrets | `GENUINE_GAP` | `crates/machine/` |
| `qworkerd` (tools host, browser CDP, terminal) | `GENUINE_GAP` | `crates/qworkerd/` |
| CLI | `GENUINE_GAP` | `crates/cli/` |
| generated contracts | `GENUINE_GAP` | `crates/contracts/`, `schemas/` |
| intelligence service + model gateway + context/knowledge/memory/skills/evaluation | `GENUINE_GAP` | `python/intelligence/` |
| desktop + web experience | `GENUINE_GAP` | `apps/desktop/`, `apps/web/` |
| native bridges | `GENUINE_GAP` | `native/macos/`, `native/windows/` |
| migrations / infra / packs / SDK / config | `GENUINE_GAP` | `migrations/`, `infra/`, `packs/`, `sdk/`, `config/` |

Non-code artifacts present at reconciliation:

| Artifact | Classification | Note |
|---|---|---|
| `DOSSIER.md`, `DOMAIN.md`, `AGENTS.md`, `IMPLEMENTATION_MASTER_PROMPT.md`, `TASKS.md`, `MANIFEST.json`, `registries/*`, `scripts/validate_v81.py` | `ALREADY_COVERED` (authority set, not product code) | Active implementation authority per DOSSIER.md §1 |
| `docs/review/2026-09-12-r2-gap-analysis.md` | not implementation | Historical review input; explicitly non-authority (DOSSIER.md §1) |

## 4. Duplicate runtimes, stores, policy/effect paths, direct provider/tool paths

None. The scan reports `CLEAN (0 findings)` over the working tree, so at this baseline there is:

- no duplicate runtime, scheduler, graph or store;
- no second policy or effect path;
- no direct provider SDK call outside a model gateway (there is no gateway yet);
- no tool registration outside the Rust Tool Registry;
- no renderer/worker access to the control database or canonical authority tables;
- no new Go code.

## 5. Canonical-owner guarantee

At baseline there is no code that can mutate durable state or cause an external effect. This is
enforced going forward, not merely asserted: `scripts/ci/inventory.py --scan` fails (exit 1) when
product code appears without a canonical owner or with a competing authority path, and
`scripts/ci/arch_check.py` (GOV-008) extends the same rule engine with ownership, dependency-cycle
and RPC-boundary checks in CI.

## 6. Consequences recorded for later tasks

- GOV-003 creates the monorepo layout; until then `crates/`, `python/`, `apps/` legitimately do not exist.
- GOV-006 closes as trivially satisfied (no legacy authority to delete).
- Every task starts from `GENUINE_GAP`; nothing needs `PARTIAL` reconciliation.
