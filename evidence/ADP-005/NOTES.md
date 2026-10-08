# ADP-005 evidence notes

Local files are built in two places. The desktop half is `apps/desktop/src/main/local-files/` (`config`, `fs-ops`, `results`,
`grants`, `roots`, `media`, `safe-edit`, `watcher`, `host`), wired by `app.ts` (native dialogs, the media protocol, window-close
revocation, the `attach` deep link), `ipc-trust.ts` (one policy row per channel), `preload/bridge.ts` (14 channels, 2 pushes),
`security.ts` (the CSP and the media response's own policy) and the composer's attach control. The agent half is
`crates/qworkerd/src/tools/scm/retention.rs` (worktree retention) and `tools/files.rs` (baseline-checked agent writes). Authority
text is DOMAIN.md §8.7 (A-G), §13.7 (A-F), §7.1 (`fs.delete.workspace`), §15 (the six `FILE_*`/`WORKTREE_*` codes) and §0 (seven
terms); values are `config/local-files.yaml` and `config/worktree-retention.yaml` with JSON schemas. `RECONCILIATION.md` records
what existed and what was touched.

Status is **PASS** (`real_boundary: false`; both dependencies, APP-003 and EXEC-006, are PASS).

## What proves each acceptance statement

| Acceptance statement | Proof |
|---|---|
| A path outside granted roots is refused | No channel takes a path (`local-files-host.test.ts`: twenty payloads that carry a path, a path where an id belongs or an unnamed field are `VALIDATION_SCHEMA` before a handler runs; a source scan finds no path-named field in any payload schema and no `node:fs` import in `host.ts`). An entry or root id the window was not given, another window's, a file used as a folder, an entry whose folder was replaced by a link leaving the root, a root replaced by a link: `FILE_OUTSIDE_ROOTS` for listing, preview, edit and watch (`local-files-roots.test.ts` 13, `local-files-host.test.ts`). Containment is proved on real paths at each use (`resolveInRoot`). Real Electron (`files.e2e.spec.ts`): folder listing, media, edit and watch refused for a folder that became a link, ids that were never issued, and every path-shaped payload. Agent side: `sandbox.rs` is unchanged and the retention host resolves its managed directory through it (a link that leaves the root is refused: `the_managed_directory_cannot_leave_the_workspace_root`). |
| A grant cannot be claimed twice or by another window | `local-files-grants.test.ts` (17): replay, another window, guessed and malformed tokens answer the same `invalid`, an expired grant is `expired` only to its owner, the timer closes descriptors with nobody asking, limits (count, per file, total, live grants) refuse whole and hold nothing open, a directory, link, FIFO and missing file are refused, the descriptor pins the picked file after the path is swapped. Through the IPC guard (`local-files-host.test.ts`): replay uploads nothing, a pop-out and the main window cannot claim each other's grant, the owner still can afterwards. Real Electron: the pop-out's claim, preview and discard of the main window's token are `FILE_GRANT_INVALID`, a media URL minted for one window does not load in another (a real `<img>` in a real second window), is refused to the main process and cannot be read with `fetch`, and claiming revokes it. |
| Cleanup is recoverable after crash | `crates/qworkerd/tests/retention.rs` (21, real Git, real SIGKILL of a child process at an exact stage of a real cleanup): killed after the prepared set is written (the same token completes after a restart), after the refs are written and after the recovery is recorded (the worktree is intact and cleanable; the person's branches, stash and index are byte-identical), after `REMOVING` is recorded and after the checkout is removed (`recover()` completes it; `restore` reproduces HEAD, the staged, unstaged, deleted and untracked state), a checkout changed after the crash is abandoned, a half-deleted checkout is finished from the snapshot, a repeated cleanup with the same token resumes. |

Also covered: every requested id is accounted for as removed or skipped with a reason, and every part of a worktree's state is
rechecked against the preview (HEAD moved, index, the content of an already-changed file, held, revision), including a change made
*during* the cleanup between the snapshot and the removal; the snapshot's shape (two parents; index snapshot built from a
temporary index, because `git write-tree` on the live index rewrote its cache extension: found by the test that holds the
worktree's index bytes constant at every stage); no branch, tag, stash or ref outside `refs/quansio/recovery/` is touched; the
sweep removes only idle, clean worktrees beyond the limit, oldest first; the effect class is held to `config/effects.yaml`;
agent `fs.write`/`fs.patch` refuse a stale `base_digest` and a concurrent change made after the temporary file is written
(`file_conflicts.rs`, 6); a person's edit is checked after the temporary file is written (a real second writer lands in the
window), backups are exclusive, permissions are kept, a self-write is silent (real `fs.watch`); the watcher shares one OS watch
per (device, inode), debounces exactly, drops on a replaced or removed directory (also with no event, on the slow check), is
bounded with a typed overflow; deep links carry no path and the attach link is the only one that opens a dialog; the shell's own data
directory is never a pick.

## Counts

`desktop-tests.log`: 55 files, 494 passed, 4 skipped (the run had the private PostgreSQL and the server binaries in its environment, so
the existing `*.e2e.test.ts` journeys ran live too; `tests/work.e2e.test.ts` is excluded as in ADP-003 and ADP-004: it asserts exact
workspace revision numbers and fails differently on different runs); `tsc` and `eslint` clean over `apps/desktop` and `tests/e2e`.
This task's files: `local-files-{config,grants,roots,media,edit,watcher,host,links}.test.ts` and `local-attach.test.ts`
(config 3, grants 17, roots 13, media 16, edit 15, watcher 18, host 21, links 3, attach 7), plus the extended `ipc-trust`,
`ipc-sources`, `security` and `preload` tests. `files-e2e.log`: the real-Electron spec, 6 passed (one `BLOCKED_EXTERNAL` line for
the object store, below). `workspace-tests.log`: `cargo test --workspace --no-fail-fast` 1099 passed, 13 failed: the same 13 calls to the
artifact/evidence object store at `127.0.0.1:59010` that ADP-002 to ADP-004 recorded (MinIO is not running); this task adds
`retention` (21, including six real SIGKILLs of a child process), `file_conflicts` (7) and the server's error-table and taxonomy
tests now cover 71 codes and 24 effect classes. `repo-tests.log`: 243 passed (`tests/architecture`, `tests/contract`, `tests/ci`;
the Rust contract generator cannot run from a worktree under `.claude/worktrees`, so `test_generated_bindings_are_current` is
run in the finalize dry-run clone). `python-tests.log`: 710 passed, 21 skipped. `lint.log`: `cargo fmt --check` and `clippy -D
warnings` clean. `static-gates.log`: validator, arch, workspace, inventory, authority, legacy map, contract regeneration (catalog,
openapi, ts, sdk) and supply chain, all clean. `shell-e2e.log` and `shell-e2e-main-control.log`: the ADP-004 shell smoke on this branch and on
main's sources, both 7 of 8 (below).

## Mutation checks

`mutation-checks.log` (script `mutation-checks.py`, run from a committed tree): 85 mutants of the guards across grants, roots and
entries, media, safe edits, the watcher, the host, the IPC policy, deep links, the CSP, worktree retention and agent writes. Every
mutant COMPILES (`tsc` or `cargo test --no-run`), the unmutated suite passes first (CONTROL-PASSED for both toolchains), and a
named test then fails: 85 killed, 0 survived, 0 no-verdict. The first full runs found real holes, each now closed by a test:
a refused grant could leak the descriptors it had opened (no count could see it: the test now counts the process's own open
descriptors), a closing window's watch, a claimed grant's media token and a released root's entry ids were not observed
directly (counts added), a save onto a link was untested at the function (now direct), an early conflict wrote a temporary file
(now asserted not to), and a temporary name that already existed was opened and truncated (the write now creates it exclusively and
goes on to the next name; the test plants links at the next forty). Where a guard has two layers (an entry's window and its root's
window, a grant's `NOFOLLOW` and the `lstat` fallback, a window's watches and its roots' watches) the mutant removes both: a
single layer is redundant by design, and a survivor of a one-layer mutant is not a hole.

## Not exercised live (stated, not claimed)

- **The native open and folder dialogs.** `dialog.showOpenDialog` cannot be operated headlessly; the real-Electron run answers the
  shell's dialog hook (`__quansioShell.hooks.dialogs`) with real paths. Everything before the dialog (the deep link, the IPC, the
  guard) and after it (grants, descriptors, media, edits) is production code. The dialog options (files only, several, one folder)
  and the parent window are not asserted.
- **The artifact bytes reaching the object store.** MinIO is not running here; the server answers the claimed file's
  `CreateArtifact` with `INTERNAL: the artifact store failed`, the e2e reports `BLOCKED_EXTERNAL` for that last hop and asserts the
  typed `upload_failed` skip instead. The upload command itself is APP-008/CORE-007's and unchanged. The same missing store makes
  `desktop.e2e.spec.ts` "reviews an artifact's versions" fail here, as in ADP-002 to ADP-004.
- **The shell smoke's indicator test.** `shell.e2e.spec.ts` "the activity indicator lists what needs the person" fails in this
  environment (the indicator lists a second, "Working", item) with the same failure when built from main's sources and the same
  server binary (a control run), so it is not caused by this task; the other seven pass.
- **Windows and Linux.** Path handling is POSIX (`O_NOFOLLOW`, `fchmod`, rename over an existing file); the code has fallbacks
  (`lstat` where there is no `O_NOFOLLOW`) that no test runs. Coarse-timestamp filesystems make the size+mtime baseline weaker;
  DOMAIN.md §13.7 D says so.
- **A real crash of the desktop.** The edit and grant state is in memory by design; a main-process crash is a restart, tested as a
  window close, not as a kill.
- **Media in a real `<video>`/`<audio>` element.** Range, HEAD and 416 are unit-tested through the handler; the real run loads an
  `<img>` only.

## Deferred

- **Re-assessed 2026-10-08, still deferred:** the call sites (create a worktree at Run start, `acquire`/`release`, `sweep()`/`recover()`
  at start-up) belong to the worker host in the execution target, because retention is a library over the target's Git and the
  server may not hold a target's worktrees; `qworkerd` has no daemon yet (QA-002 `KNOWN_GAPS`: `WorkerHost`, EXEC-002). Wiring them
  from the server would put Git state on the wrong side of the boundary, so nothing was changed.
- **The runtime's call sites for retention.** `WorktreeRetention` is a host-callable library with real-Git tests. What is not
  wired: creating a managed worktree from a Run's start, `acquire`/`release` around a Run, scheduling `sweep()` and `recover()` at
  qworkerd start-up, a command and a desktop control for a person's cleanup, and reserving the `fs.delete.workspace` effect around
  a call. The class, its tier, its reconciliation and the operation map are in `config/effects.yaml` and tested.
- **`base_digest` is accepted by `FileHost`, not yet by the tool dispatcher.** `config/tools.yaml` declares the optional field on
  `fs.write` and `fs.patch`; qworkerd's `FileHost` is not yet the thing the dispatcher calls.
- **An upload larger than the server's 8 MiB bound.** A grant may hold 50 MiB files (DOMAIN.md §13.7 A); the attach step skips a file
  over the upload bound with `too_large_for_upload`. A larger bound is a server change.
- **Remembering a granted folder across a restart** (by design nothing survives one; the person picks again).
- **A server-side `content_digest` on `CreateArtifact`** to verify the bytes from claim to store.

## Traps met

- The harness refuses a Bash command whose text it cannot show to stay inside the worktree: a heredoc script, `cd ..` before a
  git command, `. file`. Write scripts with the file tool and run them by path.
- `git write-tree` on a live index rewrites its cache-tree extension: the index "snapshot" must be built in a temporary index.
- A page's own image cache answers a repeated `<img src>` without asking the protocol handler: a revocation test needs a fresh
  query string (the token is the path; the handler ignores the query).
- `onHeadersReceived` fires for custom-scheme responses too and would replace a media response's `sandbox` policy with the page's.
- The source-scan tests strip comments with a regular expression: a string with a slash-star in it (a URL match pattern) eats the
  rest of the file, so `app.ts` spells it with an escape.
- A closing window's `evaluate` never returns: `.catch` it.
- `domain_catalog.py` read §13.7's `quansio-media` as a webhook header (its `###` sections ran to the next `##`); fixed there.
