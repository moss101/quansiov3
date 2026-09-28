# RUN-011 — an absent optional digest argument no longer refuses the call

Found by QA-001 coverage work (2026-09-27), fixed 2026-09-28 under the regression protocol (merge e93005c; evidence run on main at 51a3c63896e7, which contains every fix of this batch).

2026-09-28 (fix/RUN-011-optional-digest-args, merge e93005c, found by QA-001 coverage): config/tools.yaml declares work.delegate's work_node_id optional but digests it, and derive_params_digest required every digest argument, so a delegate call without it was refused as VALIDATION_SCHEMA. An absent optional digest argument now digests as null (the schema already enforces required arguments and refuses explicit null) and a declaration whose digest_args names a non-property fails at load. Failing tests first; control in controls.log.

Tests: cargo test -p quansio-tools --test tool_contract (an_absent_optional_digest_argument_still_plans_and_stays_distinct, a_digest_argument_must_be_a_declared_property); cargo test -p quansio-server --test turn_loop_internal_tools (a joined work.delegate without work_node_id).

Logs beside this file; controls.log reverts this task's fix on the evidence commit and shows the named test failing (KILLED) with a compiling build.
