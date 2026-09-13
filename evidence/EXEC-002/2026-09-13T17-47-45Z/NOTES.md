# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## acceptance

**Malformed/stale action envelopes fail before tool execution.** `ActionEnvelope::check_shape` and
`MachineGateway::validate` decide in a fixed order — shape, argument digest, target, lease held,
generation, lease expiry, deadline — and the test drives all eight refusals through
`WorkerHost::receive`, not through the validator directly. The executor is a counting stub, so the
assertion `calls == 0` is what proves the ordering: every refusal happened with the tool untouched. The
same test asserts the host recorded nothing for a refused action, so a refusal cannot leave a trace a
later replay would serve.

**A disconnected worker resumes or is fenced without duplicate effect execution.** Both halves are
driven through the shipped `receive`:

- *resumes*: the first delivery runs the tool and records the outcome; the re-delivery of the same
  dispatch token is answered from the record with `replayed: true` and an equal outcome, and the
  executor still shows exactly one call. A `Cancel` is recorded the same way, and its replay is likewise
  not re-dispatched (zero calls, since a cancellation runs no tool).
- *is fenced*: a replay that arrives after the lease lapsed is refused with `LeaseExpired` before the
  record is consulted — validation precedes replay — and the executor still shows one call.

A failed tool is recorded too, so a replay of a failure is also answered rather than re-run.

## recorded_decisions

- **The envelope is validated by the worker, not the transport.** `MachineGateway::validate` is pure: it
  reads no clock and touches no database, taking the lease the worker holds and the instant from the
  caller. That is what lets the same rules serve qworkerd, which has no control-plane database, and a
  test, without either re-implementing the other.
- **A dispatch token is an identity, not a bearer credential.** Presenting a live token at a generation
  the worker no longer holds is refused, because the token identifies *which* effect an action settles
  while the `(lease_id, generation)` pair is what entitles anyone to settle it.
- **The output bound is a property of the stream, not of the result.** `OutputSink` truncates each chunk
  as it arrives and counts what it discarded, so `max_output_bytes` holds for a tool that never stops
  producing. The earlier shape — run the tool, then truncate the finished buffer — left the whole
  unbounded output in the worker's memory first, which is the memory exhaustion the bound exists to
  prevent.
- **`max_output_bytes == 0` means unbounded**, matching the meaning it already had; the sink reports
  `truncated: false` in that case.
- **A heartbeat is an observation, never a claim.** `Heartbeat` carries the generation the worker holds,
  so a fenced worker cannot overwrite the observation of the generation that replaced it, and the state
  must parse as a `TargetStatus` — the controller stores it and derives health from
  `(desired_state, observed_state)` (EXEC-001), so a worker cannot declare itself healthy.
- **A private worker is never dialled.** `Dial::AwaitInbound` for `CustomerPrivateWorker` (the only
  outbound-only substrate); `Dial::Outbound` for the rest. The network-ACL test asserts all five
  substrates rather than only the private one, so a new substrate cannot default to dialling.

## the two prohibitions, and how they are enforced

`crates/qworkerd` reaches no control-plane database and evaluates no policy. Both are enforced twice:
the crate's own test scans its `src/` tree, and the repository's `worker-to-control-db` architecture
rule (`scripts/ci/arch_check.py`) independently reads the whole `crates/qworkerd/` tree. The gate is the
authority; the crate test is the companion that fails in `cargo test`.

The scan test assembles its forbidden tokens from fragments and proves its own sensitivity against a
probe file written at run time under `CARGO_TARGET_TMPDIR`. Both are necessary: the architecture gate
reads source *text*, so a literal token list in the test is indistinguishable from the violation the
test exists to catch — the gate flagged exactly that during this task (`PgConnection`, `PgPool`,
`runtime_events`, and later a quoted `"approval"`), and it was the test that was wrong, not the rule.

## what is not claimed

- The task's `real_boundary` is `false` and no provider or database boundary is involved: the gateway is
  pure and the host is in-process. The database-backed evidence in this bundle is EXEC-001's suite
  (`tests/control.rs`), re-run only to show this change did not disturb it.
- "Stream output" is implemented as a bounded, incrementally-truncating sink the tool writes into; the
  transport that chunks those bytes to the controller is not part of this task, and the envelope's
  `max_output_bytes` is the contract it will carry.
