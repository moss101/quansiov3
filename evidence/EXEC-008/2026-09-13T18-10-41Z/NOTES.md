# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## acceptance

**Unknown destinations are denied by default.** `EgressBroker`'s decision order is fail-closed: the URL
must parse, the host must not be one the broker never reaches, the target must be bound to a policy that
permits the effect class, every address the name resolved to must be in a class the policy allows, and
only then is a live grant looked for. The absence of a grant is the default answer, so a destination
nobody granted has no path to `Allow` — driven end to end through `EgressStore::decide` against real
PostgreSQL (`an_unknown_destination_is_denied_until_an_effect_grants_it`), which also shows the grant
covering that host and port only and no other.

**Policy changes fence or expire affected grants safely.** A grant records the `policies.version` and
`policy_id` it was issued under. Incrementing the version, or rebinding the target to a different
policy, is a single comparison in `decide` — no scan, no update-everything window, and no interleaving
in which a grant outlives the policy that justified it. Issuing locks the policy row `FOR UPDATE`, so a
concurrent change either lands before the grant (which then pins the new revision) or after it (which
fences the grant). Expiry and revocation are the other half: refusal is recorded, never deleted, so the
audit trail survives and a later request is refused with the reason rather than with silence.

## the three named tests

- **blocked domain** — `localhost`, `*.local` and `*.internal` are granted deliberately and still
  refused by name; a literal cloud-metadata address is refused by class. Neither half depends on the
  other.
- **DNS rebinding** — the same granted name is reached when it resolves to a public address and refused
  when it resolves to `127.0.0.1`, `10.0.0.7`, `169.254.169.254` or the IPv4-mapped `::ffff:127.0.0.1`.
  A mixed answer set is refused, because one internal answer is enough. An empty resolution is refused
  too: the addresses being unknown is not the same as their being allowed. Widening the policy to name
  the `loopback` class *does* reach it, which is what shows the refusal is the address rule rather than
  a blanket ban.
- **grant expiry** — an expired grant is refused with its own reason, expiry is exclusive at the
  instant, a window that does not move forward is refused with a named rule and writes nothing, and a
  live grant for the same destination is used in preference to the expired ones.

## recorded_decisions

- **The grant's identity is the effect that authorized it.** `egress_grants.effect_id` is the primary
  key and references `effect_records`. A grant *is* what a `network.egress.new_destination` effect
  settled, so one effect cannot acquire a second grant, and re-issuing is idempotent by construction —
  the Effect Ledger's rule rather than a rule restated here. A unique violation is mapped to the typed
  `GrantConflict`, so a cross-tenant reuse of an effect id is a refusal rather than an opaque driver
  error (the row-level-security-scoped lookup cannot see the other tenant's row).
- **The network policy is the canonical `policies` row.** `execution_targets.network_policy_id` already
  exists (DOMAIN.md §8.2), so the broker reads that row's `network.egress.new_destination` rule and
  takes `policies.version` as the revision grants are fenced on. No second policy engine, and no second
  table for something the schema already had a hook for.
- **A grant's refusal names the most recent grant, not the first one a rule order happens to match.**
  The first implementation reported the rule of whichever non-live grant sorted first, so a stale
  generation in the list masked the more useful "granted to another capability" answer; the unit test
  `the_longest_lived_live_grant_is_the_one_reported` caught it. Liveness is now looked for first and
  wins outright — an explanation is only owed when the answer is no — and the explanation comes from
  the most recently issued grant for the destination, which is the one the caller most likely means.
- **`MachineControl::set_network_policy` is the target owner's operation** (EXEC-001's module, since it
  writes `execution_targets`), so the egress tests bind a policy through the shipped entry point rather
  than writing the column directly, and the ownership scan that forbids any other module writing the
  target table still holds.
- **The decision log is built from the destination, the decision and identifiers only.** The request
  carries headers, and the record has no field for them, so a credential value cannot reach the log; the
  test supplies a canary `Authorization` header and asserts it is absent while the destination and the
  opaque credential *handle* are present, so the assertion is not vacuous.

## environment

- The dev stack is PostgreSQL on **56440** (`.env`'s `QUANSIO_DEV_POSTGRES_PORT`), reached with
  `QUANSIO_TEST_POSTGRES_URL=postgres://quansio:quansio-dev-only@127.0.0.1:56440/quansio`. Without that
  variable the suite prints `BLOCKED_EXTERNAL` and returns **passing** — which is exactly the trap this
  bundle's first run fell into; the runs recorded here all had it set, and the machine-crate run that
  reported 48 tests could not have passed without the database.

## pre-existing failures found, not caused by this task

`cargo test --workspace` is clean in 7 of 8 runs on this branch, and was clean in 8 of 10 runs on
`main` at `dd5e024` **before** this change existed, so two load-sensitive failures are pre-existing
rather than regressions. Both are in `crates/server/tests` and unrelated to egress; `workspace-tests-flake.log`
is the failing run and `workspace-tests-pass.log` the clean one.

- `tests/orchestration.rs::a_cancel_storm_cancels_each_run_once_and_blocks_dependents` reports 3
  cancelled runs where 2 exist. Root cause: `RunStore::cancel`
  (`crates/server/src/runtime/state_machine/store.rs:693`) returns `Ok(run_in_cancelled_state)` both when
  it performed the cancellation and when it found the run already cancelled, and
  `OrchestrationService::cancel_runs` (`crates/server/src/runtime/orchestration/service.rs:336`) counts
  `Ok(after) if after.status == Cancelled` as a *new* cancellation. Under a storm, the window between
  the caller's read and `cancel`'s own read lets two callers both be told they cancelled it. The event
  count is unaffected (the transaction dedupes), so this is a reporting/contract defect rather than a
  duplicated side effect — but "exactly once" is the property the test exists to assert.
- `tests/turn_loop.rs::a_turn_executes_tool_proposals_through_capability_policy_and_the_effect_ledger`
  fails with `duplicate key value violates unique constraint "protocol_states_pkey"` under full-workspace
  parallel load. It passes standalone; the same suite failed transiently during EXEC-002 as well.

## what is not claimed

- The task's `real_boundary` is `false`. The database is real, but no network boundary is exercised: the
  broker decides from addresses handed to it by the resolver, so the DNS resolution itself is the
  caller's and the enforcement at a host or network gateway is a deployment concern this task does not
  build. The resolver-provided addresses are the seam, and that seam is what the rebinding test drives.
- `crates/server/tests` is not claimed green; see above.
