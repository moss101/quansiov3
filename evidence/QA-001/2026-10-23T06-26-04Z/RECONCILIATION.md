# QA-001: TypeScript coverage measurement

Owner: scripts/ci/coverage.py (QA-001). Adds `@vitest/coverage-v8@2.1.9` (matches vitest 2.1.9; approved by the owner on 2026-10-23) and a `typescript` measurement and check. No product code changed.
Result: 64.11% of 24106 lines of apps/desktop, apps/web, sdk/typescript `src/` against the 70% floor (DOSSIER 21.4). The uncovered lines are mostly React views (renderer/*.tsx, web pages) that only the Playwright journeys (QA-008) drive; vitest's v8 run does not see those.
