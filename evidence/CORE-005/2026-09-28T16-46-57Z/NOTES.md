# CORE-005 — plan cycle check walks parent_id parent -> child

Found by QA-001 coverage work (2026-09-27), fixed 2026-09-28 under the regression protocol (merge 68dbcd7; evidence run on main at 51a3c63896e7, which contains every fix of this batch).

2026-09-28 (fix/RUN-003-parent-cycle-direction, merge 68dbcd7, found by QA-001 coverage): the GraphTransaction plan cycle check walked parent_id child -> parent (see CORE-004), so an inverted parent plan passed the plan check and only the store caught it as an untyped Graph(Cycle). It now walks parent -> child and refuses it as PlanRejection::Cycle.

Tests: cargo test -p quansio-graph --test plan_proposal (a_plan_follows_the_parent_direction_of_parent_id).

Logs beside this file; controls.log reverts this task's fix on the evidence commit and shows the named test failing (KILLED) with a compiling build.
