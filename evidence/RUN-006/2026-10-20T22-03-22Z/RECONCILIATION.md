# RUN-006 — see evidence/APP-007/2026-10-20T22-03-22Z/RECONCILIATION.md

This task's change was made in the same branch as the APP-007 live-surface work and is proved by the same gate; this bundle
names which of its logs and tests prove RUN-006. Proof for RUN-006: `policy::autonomy`, `policy::evaluation` and `identity::settings` unit tests (every mode, the strictest layer wins, a damaged layer reads as read_only, the mode is part of the decision digest) and `turn_loop.rs` `the_autonomy_mode_in_force_decides_what_an_agent_may_do_without_asking` (read_only refuses the command before any host; ask_every_action parks it; ask_for_consequences leaves tier 1 alone; standard runs both). Mutants Y8 to Y12.
