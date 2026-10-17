# EXEC-002 regression: desktop conversation and trust E2Es

Since EXEC-002 a worker tool (`fs.*`, `terminal.exec`, `process.spawn`) is refused `WORKER_UNAVAILABLE` unless a worker is
attached; it is never answered by the conformance stub. `conversation.e2e` (fs.read `rejected`) and `trust.e2e` (the fs.write
effect never settled) still expected the stub to answer, and the stub proposed absolute `/work/root/...` paths, which a worker
refuses (`PathRefusal::Absolute`).

Fix, with the real boundary rather than a simulation: the conformance stub proposes workspace-relative paths (`notes.md`,
`out.txt`; effect class stays `fs.write.host` with no known root, so the approval flow is unchanged); the hub logs
`worker attached for <target>`; both E2Es seed the `--worker` target, give the server `QUANSIO_WORKER_SOCKET` and dial a real
`qworkerd` (tests/worker.ts, `QUANSIO_TEST_QWORKERD_BIN`) whose root holds `notes.md`; they wait for the attach. The fs.read
completes on the worker's disk and the fs.write settles under its approval receipt.

Also in this change: the work E2E's remaining exact-revision assertions (`toBe(created.revision)`, state == canonical read, ...)
tolerate the orchestrator's own commits (see ADP-007 evidence 2026-10-17T08-30-00Z); the page/canonical agreement is checked
by reload-and-compare.

Environment incident: during the runs the disk filled (2.6 GiB free), Docker's containerd went read-only and every E2E failed
`cannot connect to the admin database`. Recovered per the docker-crash-recovery procedure (pruned target/debug/incremental and
stale deps, restarted Docker, `scripts/dev/up`); the 8 runs recorded here were run after recovery.

Result: 8 of 8 full desktop-suite runs: 652 passed, 0 failed, 16 skipped (other BLOCKED_EXTERNAL boundaries).
