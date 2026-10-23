# QA-001: TypeScript floor met by jsdom view tests

Owner: apps/desktop and apps/web test suites; scripts/ci/coverage.py measures. Added jsdom (dev dependency) and two rigs (`tests/dom/rig.tsx`): the desktop rig runs the real preload API over a scripted `send`; the web rig runs the real WebSession/WebClient over a scripted `fetch`. Views run their real decoders and commands. Unit-level only; no real-boundary claim.
Result: 88.91% of 24106 lines (floor 70%): desktop views 0-1% -> 91-100%, web pages 0% -> 92%. No product source changed.
Mutation checks: viewer may approve (web pages) and forget sends no command (Knowledge) both fail their tests; sources restored.
