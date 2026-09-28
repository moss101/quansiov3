# INT-005 — the WorkGraph as a work context segment

Found by QA-001 coverage work (2026-09-27), fixed 2026-09-28 under the regression protocol (merge 5696181; evidence run on main at 51a3c63896e7, which contains every fix of this batch).

2026-09-28 (fix/RUN-003-run-plan-port, merge 5696181, found by QA-001 coverage): no ContextProjection segment ever carried the WorkGraph, so a model could not name node ids or base_revision for work.propose_plan. The runtime now offers one optional `work` segment (DOMAIN.md §11.2) right after the instructions when the run's projection exposes work.propose_plan: revision, nodes and edges read through GraphStore, bounded by model_call.max_work_nodes, ref <workspace>@<revision>, hinted agent_generated when any node was not created by a user.

Tests: cargo test -p quansio-server --test served_plan_and_memory (work segment offered at the planned revision; agent_generated once a model-authored node exists).

Logs beside this file; controls.log reverts this task's fix on the evidence commit and shows the named test failing (KILLED) with a compiling build.
