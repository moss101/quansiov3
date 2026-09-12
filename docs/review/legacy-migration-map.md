# Legacy migration and deletion map (GOV-006)

**Task:** GOV-006 (M0) — Define legacy migration and deletion plan
**Status:** trivially satisfied under the greenfield rule (AGENTS.md; DOSSIER.md OD-012)
**Evidence:** `evidence/GOV-006/<ts>/summary.json`, referencing
`docs/review/2026-09-12-gov-001-reconciliation.md`.

## Finding

GOV-001's repository inventory found **no pre-existing product code**: no legacy Python/FastAPI
service, no Go service, no legacy orchestrator, no shadow state, store, policy engine, effect path,
tool registry or browser stack. There is therefore nothing to keep, port, replace or delete, and no
release-critical behavior can depend on an undocumented compatibility path.

Disposition vocabulary used by the map (AGENTS.md): `KEEP_BEHIND_BOUNDARY`, `PORT`, `REPLACE`,
`DELETE`.

## Disposition table

No legacy artifacts exist. The table is intentionally empty; `scripts/ci/legacy_map_check.py`
fails if any artifact matching a legacy/parallel-authority pattern (`legacy/`, `*.go`, `server.py`,
`orchestrator.*`, `runtime_v1.*`, …) appears without a row here.

| Artifact | Disposition | Canonical owner or replacement |
|---|---|---|

## Removal tasks

No removal tasks are required: there are no parallel orchestrators, direct provider calls, shadow
state or legacy Go authorities to remove. D-011 (no new Go code) is enforced by the
duplicate-authority scan (`new-go-code` rule) and by this map's completeness check, so a legacy Go
authority cannot be reintroduced without an explicit disposition row and an approved decision.

## Standing rule for later milestones

If any future reconciliation (a resumed session, a customer branch or an imported component) finds
legacy code, it must be added to the table above **before** it is called from a canonical owner, with
one row per module naming its disposition and the canonical V8.1 owner it is kept behind, ported to
or replaced by. A `DELETE`/`REPLACE` row must no longer resolve to an existing path, which the
checker enforces.
