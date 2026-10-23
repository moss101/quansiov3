# QA-001 fix: Collaboration add-participant refusal; onboarded field review

Owner: apps/desktop renderer. Onboarding check reviewed, not a bug: the server (crates/server/src/control/identity/auth.rs `SignedInUser.onboarded`, enterprise oidc `how == "provisioned"`) sets `onboarded` true only when the sign-in created the user, so `!onboarded` (returning user -> ready) and true (new user -> choose a target) are correct and fed; tests in identity.test.ts, app/onboarding DOM tests and server identity tests pin both. Only comments clarified; no behaviour or DOMAIN.md change.
Fix: `run()` in Collaboration.tsx caught nothing; a wrong-prefix id made participantParams throw -> unhandled rejection, no notice. It now shows the error as the notice, returns null, and the Add field keeps its text.
No persistent state, effects, contracts or migrations touched. Unit-level; no real-boundary claim.
