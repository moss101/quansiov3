# RUN-011 regression fix (with RUN-008): the turn loop's completion claim

Shared fix branch `fix/RUN-008-completion-claim-composition`; full notes in the RUN-008 bundle
of the same timestamp. RUN-011's part: the `work.complete` control tool (config/tools.yaml,
DOMAIN.md §7.5) validated through the ToolCall protocol and handed back as
`ToolDispatchOutcome::CompletionClaimed`; claims dispatched after every other call of a proposal
(`parallel::plan_rounds`); the verdict recorded as the call's result so the loop continues with
the feedback, bounded per turn by `budget_snapshot.max_verification_rounds` (default 3);
VERIFYING recovered to RUNNING; runs whose WorkNode certifies nothing are not offered the tool.
Regression: DOMAIN.md §5.6 "feedback appended and loop continues" was not implemented (the turn
ended without the model seeing feedback) and no model output could become a claim.
