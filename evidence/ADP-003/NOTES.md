# ADP-003 evidence notes

Skill recording is built in four places: the recorder (`native/macos/Sources/QuansioMacBridge/Recording/` and the
`quansio-recorder` executable), the session and archive owner (`crates/server/src/control/skills/recording/` with the
handlers in `crates/server/src/api/recordings.rs` and migration `0026_skill_recordings.sql`), the compiler
(`python/intelligence/skills/recording/`) and the desktop flow (`apps/desktop/src/recording/`, `src/main/recorder.ts`,
`src/renderer/Recording*.tsx`). Authority text is DOMAIN.md §11.7 (A-F), §9.2 (`recording.*` events), §11.5, §12 rule 7,
§14, §15. `RECONCILIATION.md` records what existed and what was touched.

Status is **BLOCKED_EXTERNAL with `implementation_complete: true`**, never PASS: the dependency ADP-002 is itself
BLOCKED_EXTERNAL (D-017, through EXEC-010: the Windows 11 UIA boundary was never run on a Windows host), and two pieces of
this task were not run against a real boundary (see Deferred).

## What proves each acceptance statement

| Acceptance statement | Proof |
|---|---|
| A recording never reaches the model unredacted | Redaction rule set v1 has three agreeing implementations pinned to one hand-written corpus (`tests/contract/fixtures/recording/redaction_corpus.json`): Rust `redact.rs` unit tests, Swift `RecordingRedactionTests` (4) and `RecorderSessionTests` (redaction runs before truncation so a cut cannot leave half a secret; the summary holds counts only; secure-field text is dropped whole), Python `test_recording_redaction.py`. The server refuses an archive that still holds anything the rules remove and stores nothing (`an_archive_that_still_holds_a_secret_is_refused_and_nothing_is_stored`: `RECORDING_UNREDACTED` names categories and JSON pointers, never text, asserted by searching the refusal body for each secret; no object and no Artifact exist afterwards and the recording is still in progress; a forged digest and an over-limit archive are refused too). The plane receives a text-only projection (`the_plane_never_receives_pixels_or_points_and_what_the_recording_says_adds_nothing`), and `accept_proposal` re-validates whatever the plane answers (`the_planes_answer_is_taken_only_after_the_servers_own_validation`). Screenshots are judged by Vision-recognised text and withheld whole (`RecordingScreenshotTests` 5, and the live capture test below). |
| A recording cannot complete without the user's explicit finish | `nothing_but_the_persons_own_finish_finishes_a_recording` (chat messages including "done" and "finish the recording" leave the recording in progress, compile and submit before the finish are `RECORDING_NOT_FINISHED`, an archive that names the model as finisher or another recording is refused and stores nothing, only the owner's own `FinishRecording` reaches `finished`, and a cancelled one is never finished), `a_service_principal_cannot_record_and_the_family_cannot_be_granted`, `an_unfinished_recording_expires_without_a_timer_and_never_completes` (lazy expiry: `aborted`, never `finished`), Swift `testNothingButFinishCompletesARecording` and `testRunningOutOfTimeAbortsAndDiscardsItNeverFinishes`, desktop `recorder.test.ts` (the recorder is finished only by a genuine click: `isTrusted` gesture) and `recording.test.ts` (the generic command path refuses recording commands). |
| A compiled skill carries no recorded coordinates or element indexes | Python `test_recording_compiler.py` and `test_recording_rpc.py`: the validator fails on any coordinate or element index anywhere in a procedure, the compiler emits bundle ids and accessibility attributes only, a click that resolved no control keeps its point only as evidence of intent in the archive and never in the procedure, input slots are left unclassified for the person, and recorded text can add no capability or instruction (it is labelled untrusted). The Rust half `a_demonstration_becomes_a_reviewable_draft_and_nothing_more` runs the real plane, and `the_archive_the_macos_recorder_produces_finishes_and_compiles_through_the_real_plane` feeds it the Swift recorder's own golden archive. |
| Owner/epoch fence on every call | `another_user_epoch_or_sign_in_is_refused_on_every_command` (all five commands, each fence axis), `a_new_sign_in_is_another_owner_but_a_token_refresh_is_not`, Swift `testEveryCallCarriesTheFence`, `testAnotherAccountOrEpochEndsTheSessionAndDiscardsEverything`, `RecorderControlTests` (11: one-time token, 0600 bootstrap file deleted after use, one authenticated channel, a stale caller ends the recording), `RecorderProcessTests` (3, the real process), desktop `recorder.test.ts` (an account change tears the recorder down at once). A stale own-session or epoch aborts a recording in progress; a stranger's call is refused without aborting it. |
| DRAFT only; promotion stays admin-only | `a_demonstration_becomes_a_reviewable_draft_and_nothing_more` (submission creates one DRAFT version through the Skill Registry; a member cannot promote it and nobody can jump a DRAFT to ACTIVE; production context stays empty), `a_submission_that_stopped_before_the_row_update_is_adopted_not_duplicated` (recovery), `start_is_idempotent_single_and_scoped_to_people_who_can_author`, `a_service_principal_cannot_record_and_the_family_cannot_be_granted`. |

Recovery: `compiling_needs_the_plane_and_the_row_survives_a_restart`, the adoption test above, the lazy expiry test.

## Real-boundary runs (this Mac, `QUANSIO_TEST_MACOS_RECORDER=1`, Accessibility, Input Monitoring, Screen Recording granted)

`swift-live.log`: `LiveRecorderTests` (3) create the real listen-only event tap and stop it cleanly, look a real control up
through the real accessibility API, and capture the real display, judge it with the real Vision recogniser and bound it;
`SecureInputTests` reads the real secure-event-input state; `RecorderProcessTests` (3) run the real `quansio-recorder`
executable (token read and deleted, a 0600 refusal, exit when its parent goes away). `apps/desktop/tests/recorder.test.ts`
drives that same real binary from the desktop host. Server tests use real PostgreSQL (private container) and the real Python
plane over gRPC. The 14 skipped Swift tests are ADP-002's `QUANSIO_TEST_MACOS_COMPUTER_USE` tests, not this task's.

## Mutation checks

`mutation-checks.log` (script `mutation-checks.py`, run from a committed tree): 49 mutants of the guards across Rust,
Swift, Python and the desktop; every mutant COMPILES and the unmutated suite passes first (CONTROL-PASSED). The full run
killed 48 and gave one NO-VERDICT: the secure-field mutant did fail its named test, but the test then indexed an empty
array and trapped the whole test process, so no failing-test line was printed. The test now reads through `first` and
`XCTUnwrap` and fails by name; the rerun at the bottom of the log kills it. None survived.

## Gates

`lint.log` (clippy `-D warnings` and fmt clean), `python-tests.log` (640 passed, 91 skipped in `python/`),
`repo-tests.log` (240 passed in `tests/architecture`, `tests/contract`, `tests/ci`; `test_generated_bindings_are_current`
is deselected here because the Rust contract generator cannot run from a worktree under `.claude/worktrees`; it is run in the
throwaway clone by the finalize dry run), `desktop-tests.log` (190 passed, 39 skipped; typecheck and eslint clean),
`static-gates.log` (validator, arch, workspace, inventory, authority, legacy map, supply chain, contract regeneration:
all clean).

`workspace-tests.log`: `cargo test --workspace --no-fail-fast` gave 1067 passed, 13 failed. All 13 are calls to the
artifact/evidence object store at `127.0.0.1:59010` (MinIO was not running, `ObjectStore(Transport(...))` or "the
artifact store failed"): `api` RLS surface, `artifact_workflows`, `artifact_workspace`, `artifacts`, a `conversation_surface`
upload and a `trust_surface` evidence digest test. None touches this task; ADP-002's bundle recorded the same 13. They are
BLOCKED_EXTERNAL here, not skipped or weakened.

## Deferred

- **Live narration.** The transcript channel, its redaction and its ingest are built and tested, but no live speech capture
  is wired (it needs Speech Recognition and Microphone permission and was not built unverified). A recording made today has
  no voice transcript.
- **The S3 object store path.** The API tests keep archives in an in-memory object store because the MinIO image was not
  available offline; the artifact suite owns the S3 path and was not runnable here.
- **User-data export and deletion of recordings.** No `DeleteUserData` / `ExportTenantData` handler exists yet; recordings are
  ordinary owner-only Artifacts and will be reached by them when it does.
- **The Windows recorder.** The platform seam (`ObservationSource`) is in place; macOS only (spec E).
- **ADP-002 / EXEC-010** remain BLOCKED_EXTERNAL, which is why this task cannot be PASS.

## Traps met

- The supply-chain gate reads any `skill`-named YAML as a skill manifest: the recording config is `config/recording.yaml`.
- `arch_check` forbids `setInterval` (a second scheduler): desktop polling is self-rescheduling `setTimeout`.
- A new event family needs its `EventFamily` variant, or the runtime refuses the event type ("unknown event family").
- A shared cargo target can hold a stale `core`/`events` crate: touch `id.rs` and `event_type.rs` after adding ids or families.
- A Swift `DispatchSource` held in a local is dropped and never fires: the parent-exit watch is a global.
- An XCTest process that traps (index out of range) prints no failing-test line, so a mutation harness reads it as NO-VERDICT:
  read arrays through `first` / `XCTUnwrap` in tests.
- The Docker VM disk was full at one point; a tmpfs PostgreSQL container served the private database.
- `python/tests` must run from `python/` (two tests read relative paths); the repo-level suites run from the root.
