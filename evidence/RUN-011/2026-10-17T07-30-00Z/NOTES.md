# RUN-011 regression: debug turn path overflowed a default 2 MiB test thread stack

Failing tests: `completion_claims`, `routines`, `served_plan_and_memory` aborted with a stack overflow in a debug build
on main (exposing commit 9b81df75); they passed only with RUST_MIN_STACK=16777216.
Cause: `api::commands::dispatch` kept every command arm's future in one poll frame (~756 KB), and the engine turn,
tool dispatch (`run_to_result`) and `continue_run` held their large children inline. Each is now `Box::pin`ned at the await.
Result: the served claim path passes at 1.25 MiB (it failed at 1.5 MiB before). Guard:
`completion_claims::the_served_claim_path_fits_a_thread_stack_below_the_default` runs it on a 1.5 MiB thread.
Status stays PASS (no committed transient FAIL). Dev DSN not recorded.
