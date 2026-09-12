# Quansio V8.1

Quansio is a persistent, governed AI work platform: a user converses with a persistent teammate,
creates long-running objectives, delegates to scoped workers, researches with evidence, operates
browser and native applications, uses files/terminal/connectors, produces durable artifacts,
collaborates with humans and agents, schedules recurring work, and retains governed knowledge,
skills and Business Capabilities.

The governing invariant is: **the model proposes; the trusted runtime owns reality.**

## Implementation authority

Only these artifacts are implementation authority (DOSSIER.md §1):

| Artifact | Role |
|---|---|
| `DOSSIER.md` | product, architecture, ownership, language, technology and release rules |
| `DOMAIN.md` | canonical domain model: names, identities, fields, states, transitions, effect classes, capability algebra, command catalog, error taxonomy |
| `AGENTS.md` | binding implementation behavior for human and coding agents |
| `IMPLEMENTATION_MASTER_PROMPT.md` | agent bootstrap (must not contradict the authority set) |
| `registries/tasks.json` | canonical task definitions and dependencies |
| `registries/progress.json` | mutable implementation status and evidence index |

Generated views — never hand-edited: `TASKS.md`, `registries/task-graph.json`, `MANIFEST.json`
(regenerate with `python3 scripts/validate_v81.py --write`).

Superseded implementation documents under `docs/archive/` and review reports under `docs/review/`
are **historical input only** and must never be consumed as current task authority. See
`docs/archive/README.md`.

## Language boundaries (locked)

- **Rust** — trusted authority and execution plane: runtime, graphs, policy, capabilities, Effect
  Ledger, tools dispatch, machine control, `qworkerd`, CLI, indexer.
- **Python** — intelligence plane behind generated typed RPC: model gateway, context, knowledge,
  memory, embeddings, skills, evaluation, capability compiler.
- **TypeScript/React** — product experience: desktop, web, SDK; never authoritative state.
- **Native (Swift/Windows)** — narrow OS-API bridges only.
- **Go** — not a V8.1 development language (D-011).

## Repository layout

`crates/` (server, core, graph, events, capability, tools, indexer, machine, qworkerd, cli,
contracts), `python/intelligence/`, `apps/desktop`, `apps/web`, `native/macos`, `native/windows`,
`schemas/`, `migrations/`, `config/`, `packs/`, `sdk/`, `infra/`, `tests/`, `evidence/`, `scripts/`
— full mapping in DOSSIER.md §17.

## Working on this repository

```bash
bash scripts/dev/bootstrap.sh              # every gate below, in order
python3 scripts/validate_v81.py            # authority gate; must PASS before and after changes
python3 scripts/validate_v81.py --ready    # dependency-ready tasks
python3 scripts/validate_v81.py --next     # next task to claim
python3 scripts/ci/inventory.py --scan     # duplicate-authority / language-boundary scan
python3 scripts/ci/check_authority.py --check
python3.12 scripts/ci/workspace_check.py   # canonical owner ↔ package conformance
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
(cd python && uv sync --frozen && uv run --frozen ruff check . && uv run --frozen mypy intelligence && uv run --frozen pytest -q)
uv run --project python pytest tests -q    # repository-level architecture tests
pnpm install --frozen-lockfile && pnpm build && pnpm typecheck && pnpm test && pnpm lint
swift test --package-path native/macos     # macOS bridge (macOS only)
```

Task loop (`AGENTS.md`): `CLAIM → RECONCILE → PLAN → IMPLEMENT → MIGRATE → TEST → NEGATIVE TEST →
RECOVERY TEST → EVIDENCE → VALIDATE → UPDATE PROGRESS → NEXT`, one task per
`task/<TASK-ID>-<slug>` branch merged to `main` with a merge commit recorded in
`registries/progress.json`.

Current position, ready queue and resume instructions live in `HANDOFF.md`.
