# QA-008 — desktop journeys on the refreshed desktop design

APP-003's design refresh (merge 44f4e47) changed the sidebar footer: a single pill switches the
sidebar between Chats and Workspace, replacing the toggle group the harness waited on. The harness
(`tests/e2e/harness.ts`) now treats the Account button as the signed-in shell and switches to
Workspace only when the Surfaces list is not already shown. No journey assertion changed.

Runs against the dev stack (real Electron, real server):
- `desktop-journeys-first-run.log`: 11/12; "onboards a new person" timed out waiting for the
  Security defaults heading after choosing a target. It did not reproduce: three isolated runs of
  that journey and two full runs (`desktop-journeys-run1.log`, `-run2.log`) passed 12/12. Recorded
  here rather than dropped.
- The accessibility and keyboard journey passes on every surface of the new design, including the
  composer (its focus ring is on the pill around the field).
