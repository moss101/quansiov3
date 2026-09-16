# GOV-005 fix — all 14 ESLint findings; toolchains gate fully green

Second half of the `toolchains` gate fix (first half:
`evidence/GOV-005/2026-09-16T19-09-58Z/`, the PyYAML/ruff issue). After that
fix, `pnpm lint` — the last step of `bash scripts/dev/bootstrap.sh` — had a
separate, pre-existing, unrelated failure: 14 ESLint problems across 4 files.
User-requested follow-up after the investigation report flagged it.

## Findings and fixes

**1. `apps/desktop/src/artifacts/workspace.ts:59`** — `previewExecutesWithHostPrivileges`'s
`_mediaType` parameter is genuinely unused (the function always returns
`false`). Kept deliberately for call-site/test legibility ("for `text/html`
specifically, is it privileged? no" — see `apps/desktop/tests/artifacts.test.ts`)
and signature symmetry with the sibling `previewMode`/`previewIsSafe` functions,
which do use their `mediaType`. Documented why and added one scoped
`eslint-disable` — the first in this codebase, which otherwise has zero
suppressions anywhere. Considered and rejected: dropping the parameter (breaks
the test's "for this type" phrasing and the symmetry), widening the lint
config's `argsIgnorePattern` repo-wide (this is the only `_`-prefixed
identifier in the whole `apps/`/`sdk/` tree — no existing convention to
formalize, and this codebase's demonstrated preference elsewhere is strict
config with a scoped, documented exception over a loosened default).

**2. `apps/desktop/src/main/security.ts:48-52`** (10 errors: 5×
`no-unnecessary-boolean-literal-compare` + 5× `no-unnecessary-condition`) —
`rendererSecurityBaselineHolds()` compared each `RENDERER_WEB_PREFERENCES`
flag to a boolean literal (`=== false` / `=== true`). With
`RENDERER_WEB_PREFERENCES` typed via `as const`, TypeScript already narrows
each flag to its exact literal type, so the comparison is statically vacuous —
ESLint was correctly reporting that the check proves nothing the type system
did not already know. Root-caused rather than restyled: the type was too
narrow for what this function actually needs, a genuine **runtime** regression
guard (its own doc comment: *"a regression that re-enables Node in the
renderer fails here rather than in a screenshot"*). Two mechanical rewrites
(`!x`/`x` for style, `=== false`/`=== true` for style) were tried and each
only swapped which of the two rules fired — neither addresses the root cause,
which is the type being too narrow for the check to mean anything. Fixed by
replacing `as const` with an explicit `RendererWebPreferences` interface
typing every flag `boolean`; the shipped values are byte-identical, but the
checks are now real assertions, satisfied cleanly by direct truthiness with no
suppression anywhere. Checked both other consumers of the constant —
`shell.ts` (`readonly webPreferences: typeof RENDERER_WEB_PREFERENCES`) and
`index.ts` (re-export only) — neither depends on the literal narrowing that
was removed.

**3. `apps/desktop/tests/diagnostics.test.ts:30`** — `` `line ${i} ...` `` where
`i` is `Array.from`'s `number` index; `@typescript-eslint/restrict-template-expressions`
(part of the `strictTypeChecked` preset) requires an explicit conversion.
Wrapped in `String(i)` — the rule's own suggested fix; `${i}` and
`${String(i)}` produce byte-identical output, so this is a pure type-safety
annotation with no behavior change.

**4. `tests/e2e/journeys.spec.ts` + `tests/e2e/vitest.config.ts`** — neither was
covered by any `tsconfig.json`, so ESLint's type-aware "project service"
refused to lint them at all (`was not found by the project service`).
`tests/e2e/` is **not** a pnpm workspace package (`pnpm-workspace.yaml` lists
only `apps/*` and `sdk/typescript`), so none of the three existing
tsconfigs' `include` reached it, and nothing pre-existing claimed these files.
Added `tests/e2e/tsconfig.json`, matching the exact `extends`/`noEmit` pattern
already used by `apps/desktop`, `apps/web` and `sdk/typescript`'s own
`tsconfig.json`.

Once type-aware linting could actually run on the file, it surfaced a real,
separate, previously-invisible finding in the same file: the dynamic
`import("playwright")` in the QA-008 real-boundary Playwright tier resolves to
`any` — `playwright` is deliberately not a workspace dependency (it is
installed only on a host that actually runs the served-build tier under
`QUANSIO_TEST_E2E=1`) — cascading into 13 `no-unsafe-*` errors on every
subsequent property access. Rather than suppress those broadly, added
`tests/e2e/playwright.d.ts`: a minimal ambient module declaration for exactly
the three calls this one file makes (`chromium`/`firefox`/`webkit`.`launch()`,
`Browser.newPage()`/`.close()`, `Page.goto()`/`.title()`). With real types in
place, `tsc -p tests/e2e/tsconfig.json` is clean and **no eslint-disable is
needed anywhere in this file** — a first attempt used a scoped
`eslint-disable`/`eslint-enable` pair around the block, which correctly
silenced the errors but then itself triggered `unused eslint-disable
directive` once the ambient types made the block provably safe; removed in
favor of the real fix.

**Side finding, not fixed here:** running `tests/e2e/` directly confirms it
works — `npx vitest run --config tests/e2e/vitest.config.ts` passes 7 tests (1
correctly skipped, the real-boundary tier gated on `QUANSIO_TEST_E2E`). But
because it is not a workspace package, **it is not invoked by `pnpm test` /
`pnpm -r`, and was never running as part of any CI gate** — QA-008's own
critical-journey suite (onboarding → chat → approval → artifact → live
takeover → teammate roster → i18n → offline reconnect → accessibility
baseline) has been sitting in the tree, correct and passing, but silently
disconnected from the pipeline. Flagged to the user as a separate QA-008
finding (wiring it into `pnpm test`/`ci_summary.py` is a test-coverage
decision, not a lint fix) rather than fixed unilaterally in this commit.

## Verification

- `pnpm lint` — 0 problems (was 14).
- `pnpm build` / `pnpm typecheck` / `pnpm test` — all clean; 33 tests across 3
  packages (`apps/desktop`, `apps/web`, `sdk/typescript`), unchanged pass
  count from before this fix (no regression).
- `npx tsc -p tests/e2e/tsconfig.json --noEmit` — clean.
- `npx vitest run --config tests/e2e/vitest.config.ts` — 7 passed, 1 skipped
  (as designed).
- `bash scripts/dev/bootstrap.sh` end-to-end — **`bootstrap: OK`** (Rust +
  Python + TypeScript + Swift), the first clean run of this gate this
  session.
- `bash scripts/ci/ci.sh` on the merge commit — **`pipeline: PASS`, 11/11
  gates**, the first fully green baseline-pipeline run recorded this session
  (`final-ci-run.log`).

## Files changed

`apps/desktop/src/artifacts/workspace.ts`, `apps/desktop/src/main/security.ts`,
`apps/desktop/tests/diagnostics.test.ts`, `tests/e2e/journeys.spec.ts` (four
lines of comment cleanup), `tests/e2e/tsconfig.json` (new),
`tests/e2e/playwright.d.ts` (new).
