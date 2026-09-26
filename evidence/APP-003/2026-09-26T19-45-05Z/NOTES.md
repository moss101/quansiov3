# APP-003 — Settings keeps what the person typed while it reads the current values

Found while testing the theme preference: Settings read the effective values from `/v1/me` on
opening and after every save and replaced every field with them, so a choice made before the read
arrived — or while a save was in flight — was reset and then saved as the old value.

Fix (`apps/desktop/src/settings-form.ts`, `renderer/Settings.tsx`): a read fills only fields the
person has not changed since they were last saved (`mergeLoaded`); a save clears the edits it sent
unless they changed again meanwhile (`afterSave`); the Save buttons wait for the first read, so
nothing unread (an empty workspace name) is saved; the administrator-only note waits for the role.

Tests: `settings-form.test.ts`; a session journey chooses a theme at once on opening Settings and
sees it saved, with another form's unsaved edit surviving both reads; the theme journey no longer
waits for the read. With reads overwriting edits both journeys fail; the controls pass. Desktop
suite `desktop-suite.log` 160/160 (serial, dev stack). QA-008 desktop journeys: run 1 had one
failure — `electron.launch` timed out after 60 s starting a freshly rebuilt app (the onboarding
journey; nothing it drives changed) — runs 2 and 3 passed 12/12.
