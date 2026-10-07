# ADP-007 plan

- [x] A. Inbound triggers: `webhook` trigger kind, endpoint + secret (broker, shown once, rotation), HMAC verification with
      tolerance, replay ledger (delivery key + signature digest), 64 KiB cap, per-endpoint rate limit, typed filters, fire claimed
      before execution with the payload kept on it, payload as an `untrusted_data` block through the INT-012 segment path.
- [x] B. Management: `expected_revision` (full replacement), `create_request_id`, per-plan limits from `config/automation.yaml`,
      `DryRunRoutine` (pure, rate limited), Automations copy that says a trigger or script is the tool for waiting.
- [x] C. IANA time zones with explicit gap / repeat rules; unknown names fail closed (`AUTOMATION_TIMEZONE_UNKNOWN`).
- [x] D. Workflow scripts: IR and validation, compile through the Planner, journal as Evidence, executor (resume, no repeated
      call), gates through the approval path, budget pause into an attention item, schema-validated item results. A worker turn a
      killed process left active is recovered (the in-flight model step fails, the turn aborts, the model is asked again).
- [x] E. Desktop: Automations gains hook entries (endpoint shown once), dry run, revision conflict handling, IANA picker and the
      script view with its budget.
- [x] Served recovery: the real `quansio-server` process killed with SIGKILL and relaunched (hook fire; script run).
- [x] Evidence: tests, mutation checks, NOTES, summary.json, finalize script.

Tests per acceptance statement:

| Statement | Test |
|---|---|
| A replayed webhook does not fire twice | `automation_triggers::a_replayed_webhook_does_not_fire_twice` (real listener, restart); `automation_served::a_hook_fire_claimed_before_a_kill_runs_once_after_the_relaunch_and_its_replay_is_a_duplicate` (killed and relaunched process) |
| Payload text can never authorize an action | `automation_triggers::the_payload_reaches_the_run_only_as_untrusted_data...`; `automation_untrusted::payload_text_cannot_authorize_an_action_through_the_real_plane` |
| A restart resumes a script from its journal without repeating completed calls | `automation_workflow::a_restart_resumes_a_script_from_its_journal_without_repeating_a_completed_call`; `automation_served::a_killed_process_resumes_a_hook_fired_script_from_its_journal_without_repeating_a_completed_call` |
| Named: webhook dedupe | `a_replayed_webhook_does_not_fire_twice` |
| Named: stale-revision update | `a_stale_revision_is_refused_and_a_full_replacement_replaces_everything` |
| Named: DST fire | `a_scheduled_window_fires_once_across_a_spring_forward_gap_and_a_fall_back_repeat`; `scheduler::routine` / `scheduler::timezone` unit tests over New_York, London, Auckland, Lord_Howe, Kolkata, Sao_Paulo |
| Named: journal replay / restart | `automation_workflow::a_restart_resumes...`, `automation_served::a_killed_process_resumes...` |
| Named: budget approval | `above_the_threshold_a_script_holds_an_approval_before_it_fans_out`, `reaching_the_call_budget_pauses_the_run_into_an_attention_item_instead_of_failing_it` |
