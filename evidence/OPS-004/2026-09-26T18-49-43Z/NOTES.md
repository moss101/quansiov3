# OPS-004 — the usage store passes clippy (correcting the previous evidence)

The previous OPS-004 bundle (`evidence/OPS-004/2026-09-26T18-36-48Z`) recorded `lint.log` as passing,
but its content is `error: could not compile quansio-server (lib test)` followed by `exit=0`: the exit
status captured was the `tail` that filtered clippy's output, not clippy's own. Clippy failed on
`unnecessary_sort_by` in `crates/server/src/usage/store.rs`, and `main` failed the clippy gate from
merge 67e20d6 until this fix. Found when the next branch (CORE-008) ran clippy.

Fix: `sort_by_key(|record| Reverse(record.occurred_at))` — the same newest-first order. Clippy's
status is now captured before its output is filtered (`lint.log`: `fmt_exit=0 clippy_exit=0`). The
usage store and web admin suites pass unchanged (`tests.log`).
