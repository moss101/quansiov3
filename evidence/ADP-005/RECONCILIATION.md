# ADP-005 reconciliation

Recorded before implementation (AGENTS.md task loop, RECONCILE) and updated at the end with what was and was not done.
Coverage: `PARTIAL`. APP-003 gave the desktop a sandboxed renderer, a preload allowlist, deep links and an upload that
carries bytes the renderer read from an `<input type=file>`; ADP-004 gave per-window IPC trust and the `schema.ts` payload
validators; EXEC-006 gave qworkerd a sandbox (`SandboxPath`) and `fs.read/write/patch`, EXEC-012 local Git reads. There was
no native file grant, no granted root, no media protocol, no baseline-checked local write, no file watcher and no worktree
retention.

## Canonical owner

`Product` (task `owner`); paths from the task: `apps/desktop/src/main/`, `crates/qworkerd/src/tools/`,
`crates/server/src/artifacts/`.

- Desktop (TypeScript): `apps/desktop/src/main/local-files/` (new directory inside the owner path): `limits` (config),
  `fs-ops` (descriptor-based file operations), `grants`, `roots` (roots + entry ids), `media`, `safe-edit`, `watcher`,
  `host` (the IPC handlers). Wiring: `app.ts` (dialogs, the media protocol, window-closed revocation), `ipc-trust.ts`
  (policy rows), `deeplinks.ts` (the `attach` intent, path-carrying links refused), `security.ts` (CSP `img-src`/`media-src`
  allow the one scheme), `preload/bridge.ts` (channels), and the composer's attach control in the renderer.
- qworkerd (Rust): `crates/qworkerd/src/tools/scm/retention.rs` (new; worktree retention and recovery) and
  `crates/qworkerd/src/tools/files.rs` (baseline-checked `write_checked`/`patch_checked`, atomic patch).
- Server: `crates/server/src/artifacts/` needs no change. A claimed file reaches the Artifact store through the existing
  `CreateArtifact` upload (`origin: upload`, 8 MiB bound). Outside edits an authority text forces: `api/error.rs` (the six
  new §15 codes; a test holds the enum to `schemas/catalog/errors.yaml`) and `effects/taxonomy.rs` (the new effect class).

## Existing code and its coverage

| Code | Classification | What it did | What changes |
|---|---|---|---|
| `main/ipc.ts` `uploadAttachment`, `DesktopFiles` | covered, reused | `CreateArtifact` from bytes the renderer supplied; save/open through native dialogs | the grant's attach step calls the same `uploadAttachment`; nothing about it changes |
| `main/deeplinks.ts` `parseDeepLink` | covered, extended | typed intents; any query is refused except the OAuth callback | gains `attach-files`; tests pin that no path-like parameter, fragment or `file:` value is ever accepted |
| `main/ipc-trust.ts` (`POLICY`, `IpcGuard`) | covered, extended | per-window-type channel table, exhaustive | one row per new channel; a pop-out may use the grant channels for its own thread only |
| `main/schema.ts` | covered, reused | payload validators | every new channel validates through it |
| `main/windows.ts` (`WindowManager.onWindowEvent`) | covered, reused | window lifecycle events | `closed` revokes grants, roots, entries, edit sessions, media tokens, watches |
| `main/security.ts` CSP | covered, extended | one CSP for every response | `img-src` and `media-src` also allow `quansio-media:`; the test that pins the CSP is updated |
| `qworkerd/tools/sandbox.rs` (`Sandbox`, `SandboxPath`) | covered, reused | root-relative path proof incl. symlink escape | the retention host resolves its managed root with it |
| `qworkerd/tools/files.rs` (`write`, `patch`) | partial, extended | atomic write (fixed temp name); `patch` wrote in place after reading | `write_checked`/`patch_checked` re-check the digest after the temp file is written and just before the rename (`FILE_EDIT_CONFLICT`); `patch` becomes atomic; the old methods delegate with no baseline |
| `qworkerd/tools/scm/mod.rs` (`Git`) | covered, reused | offline status/diff/branch/commit/worktree list through `git -C` | retention drives `git` directly with its own environment (temporary index, fixed identity); `Git` is unchanged |
| `config/effects.yaml`, `config/tools.yaml` | covered, extended | taxonomy; tool declarations | `fs.delete.workspace`, `worktree_operations`; optional `base_digest` on `fs.write`/`fs.patch` |

## Persistent state touched

- Server: none. No table, no migration, no row.
- Desktop: none persisted. Grants, roots, entry ids, media tokens and edit sessions live in main's memory and end with the
  process (they must not survive a restart). The person's own preferences file is not touched.
- Target (qworkerd): per managed worktree one recovery record file `<managed root>/.quansio/worktrees/<id>.json` and one
  prepared-set file `.quansio/prepared/<token>.json` (deleted when the cleanup is fully accounted for), plus Git refs under
  `refs/quansio/recovery/<id>/`. These are the target's own bookkeeping inside its managed root, not a service or a store
  (D-019); the refs are Git's own durable object store.

## Contracts and events changed

DOMAIN.md: §0 (seven terms), §7.1 (`fs.delete.workspace`), §8.7 (new, normative A-G), §13.7 (new, normative A-F), §15
(`Files` family, six codes and their flags). No command, no event, no read projection: the desktop channels are client
behaviour like §13.6, and retention is host-callable, not a Tool the model sees (the model-visible/host-callable split of
ADP-008 applies; nothing here is registered as a model tool). `scripts/ci/domain_catalog.py`: `### ` sections now end at the
next `### ` (the §13.5 webhook-header scan was reading `quansio-media` out of §13.7) and an error family may list three
statuses.

## External effects and capabilities

- Reading a picked file, listing a granted root, previewing: no Effect Ledger effect (local reads of what the person chose).
- Saving a local file edit: the person's own edit in their own folder, not an agent effect; baseline-checked and atomic.
- Attaching: the existing `CreateArtifact` (`content.upload`, tier 1) from the person's click.
- qworkerd `fs.write`/`fs.patch`: unchanged classes (`fs.write.workspace`/`fs.write.host`); the new optional `base_digest` only
  adds a refusal.
- Worktree removal: new class `fs.delete.workspace` (tier 2, ask; a sweep of fully clean managed worktrees allow+log),
  reconciliation by querying the path and the recovery record. Honest reasoning is in DOMAIN.md §8.7 G: the Git-visible state
  is restorable from the recovery refs, ignored files are not captured, so it is not tier 1; it is restorable, so it is not
  tier 3. Prepare is `read.internal`; creation and restore are `fs.write.workspace`.
- No capability is widened: a renderer gains no path, no fs read and no new window-type reach.

## Real boundaries

`real_boundary=false`. Real filesystems (temporary directories, real symlinks, hard I/O errors), real `git` (real
repositories and linked worktrees, including a SIGKILL of a child process mid-cleanup) and the real Electron binary
(native-dialog hook only; the protocol, CSP, preload and IPC are real). Not exercised live (see NOTES.md): the native
open/folder dialogs themselves (a hook answers them), Windows and Linux filesystems.

## Migrations and rollback

No migration. Rollback: revert the task branch; `config/local-files.yaml: local_files.enabled: false` turns the desktop half
off without a build. Recovery refs are inert Git refs and can be deleted with `git for-each-ref refs/quansio/recovery | git update-ref -d`.

## Plan (checklist)

- [ ] DOMAIN.md, configs, schemas, error enum, taxonomy, regenerated contracts; contract tests.
- [ ] qworkerd: `files.rs` baseline-checked write/patch; `scm/retention.rs`; tests incl. real SIGKILL recovery.
- [ ] Desktop: `local-files/*` modules; `app.ts`/`ipc-trust.ts`/`deeplinks.ts`/`security.ts`/preload; renderer attach control.
- [ ] Tests: grants (claim, expiry, replay, other window, limits, symlink, directory), media (revocation, range, method),
      edit conflict (race after temp write), watcher (confinement, debounce, inode change, overflow, self-write), retention
      (two-phase, accounting, never touches user refs, crash recovery), source scans, real Electron.
- [ ] Mutation checks; evidence bundle; validator; finalize script.
