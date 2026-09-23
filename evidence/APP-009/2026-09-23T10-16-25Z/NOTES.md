# APP-009 regression: concurrent messages to one teammate

**Found:** APP-015's full CI (`artifacts/ci/logs/b1b61e93c803/toolchains.log`, excerpt in
`e2e-before-ci-excerpt.log`): `apps/desktop/tests/collaboration.e2e.test.ts` — "keeps one
order for two clients posting at once" — failed with `RUNTIME_ILLEGAL_TRANSITION: illegal
agent_thread transition: ACTIVE -> ACTIVE`. Rerun alone on that branch it failed 2 of 6 times.
It had passed in APP-009's and APP-010's CI runs: an intermittent race, not a new behaviour.

**Cause:** `PostMessage` routing (`api/commands.rs::route_to_teammate`) provisions the
teammate's agent thread and, when it is `PROVISIONED` or `SUSPENDED`, activates it from the
row it read. Six concurrent posts to a new group thread read the same `PROVISIONED` row; the
first activation wins and the rest are refused as `ACTIVE -> ACTIVE`, failing the post.

**Fix (f32d3e5):** `AgentStore::ensure_active` (the agent-thread owner) activates an inactive
thread and, when the activation is refused because a concurrent caller already moved it,
re-reads the thread and accepts it if it is `ACTIVE`; otherwise it returns the original error.
The routing path uses it.

**Tests:** `crates/server/tests/agents.rs::ensure_active_accepts_a_thread_another_caller_already_activated`
shows the plain activation from a stale read is refused (the race), `ensure_active` accepts
it, six callers racing from one read all succeed with exactly one `agent.activated`, and a
terminated thread is still refused. The collaboration E2E passed 8 of 8 runs after the fix
(`e2e-after-8-runs.log`); the agents, collaboration and conversation-surface suites pass.
