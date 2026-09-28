# CORE-004 — parent_id walked in the parent_of direction

Found by QA-001 coverage work (2026-09-27), fixed 2026-09-28 under the regression protocol (merge 68dbcd7; evidence run on main at 51a3c63896e7, which contains every fix of this batch).

2026-09-28 (fix/RUN-003-parent-cycle-direction, merge 68dbcd7, found by QA-001 coverage): edge_would_cycle_tx walked work_nodes.parent_id child -> parent while parent_of edges run parent -> child, so restating a parent as a parent_of edge was refused as a cycle and a child could depend on its own parent while a parent could not depend on its child. The recursive check now walks parent_id parent -> child; DOMAIN.md §4.2 states the direction. The existing parent-chain assertion was corrected to the child depending on its parent.

Tests: cargo test -p quansio-graph --test cycle_rejection (parent_of_and_parent_id_share_one_direction).

Logs beside this file; controls.log reverts this task's fix on the evidence commit and shows the named test failing (KILLED) with a compiling build.
