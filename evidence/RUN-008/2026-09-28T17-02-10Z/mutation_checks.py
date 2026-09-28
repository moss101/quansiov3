"""Mutation checks for the completion-claim fix: each mutant must compile and fail its test."""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path("/Users/mohsin/projects/quansiov3/.claude/worktrees/xenodochial-haslett-0efecc")
MUTANTS = [
    ("M1 verifier not composed", "crates/server/src/api/commands.rs",
     "        .with_verification(Arc::new(verifier))\n", "",
     "a_claim_whose_contract_passes_succeeds_the_run_through_the_independent_verifier"),
    ("M2 no contract narrowing", "crates/server/src/intelligence/model_source.rs",
     "!offers_completion(&self.pool, &request.tenant_id, &run.work_node_id).await?",
     "(offers_completion(&self.pool, &request.tenant_id, &run.work_node_id).await? && false)",
     "a_plain_conversation_is_never_offered_a_completion_claim_and_stays_open"),
    ("M3 rejection ends the turn", "crates/server/src/runtime/state_machine/engine.rs",
     "if tally.rejected < tally.budget.max_verification_rounds {",
     "if tally.rejected < tally.budget.max_verification_rounds && false {",
     "a_failing_contract_returns_its_feedback_to_the_model_and_the_turn_continues"),
    ("M4 no per-turn bound", "crates/server/src/runtime/state_machine/engine.rs",
     "if tally.rejected < tally.budget.max_verification_rounds {",
     "if tally.rejected < tally.budget.max_verification_rounds || true {",
     "rejected_claims_are_bounded_per_turn_and_leave_the_work_incomplete"),
    ("M5 VERIFYING not recovered", "crates/server/src/runtime/state_machine/engine.rs",
     "let run = if run.status == RunStatus::Verifying {",
     "let run = if run.status == RunStatus::Verifying && false {",
     "a_run_stopped_mid_verification_recovers_to_running_and_can_claim_again"),
    ("M6 claimant model unnamed", "crates/server/src/runtime/verification/service.rs",
     "                claimant_model,\n",
     "                claimant_model: claimant_model.filter(|_| false),\n",
     "a_verifier_that_is_the_claimant_cannot_certify_a_contract_requiring_independence"),
    ("M7 verdict not recorded on the call", "crates/server/src/runtime/turn_loop/dispatch.rs",
     'self.update_tool_call(tool_call_id, "completed", None, Some(result))',
     'self.update_tool_call(tool_call_id, "completed", None, None::<Value>.filter(|_| result.is_null()))',
     "a_failing_contract_returns_its_feedback_to_the_model_and_the_turn_continues"),
]


def run(test: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["cargo", "test", "-p", "quansio-server", "--test", "completion_claims", "--", "--exact", test],
        cwd=ROOT, capture_output=True, text=True, env=os.environ.copy(),
    )


def main() -> int:
    ok = True
    for name, rel, old, new, test in MUTANTS:
        path = ROOT / rel
        source = path.read_text()
        assert source.count(old) == 1, (name, source.count(old))
        path.write_text(source.replace(old, new))
        try:
            result = run(test)
        finally:
            path.write_text(source)
        out = result.stdout + result.stderr
        compiles = "error[" not in out and "could not compile" not in out
        failed = "test result: FAILED" in out and "1 failed" in out
        verdict = "KILLED" if compiles and failed else ("NO-COMPILE" if not compiles else "SURVIVED")
        ok &= verdict == "KILLED"
        print(f"{name}: COMPILES={compiles} test={test} -> {verdict}", flush=True)
    # Control: the unmutated tree passes every test the mutants targeted.
    for test in sorted({m[4] for m in MUTANTS}):
        result = run(test)
        passed = "test result: ok. 1 passed" in result.stdout
        ok &= passed
        print(f"CONTROL {test}: {'PASS' if passed else 'FAIL'}", flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
