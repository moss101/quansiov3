# RUN-010 / OPS-004 — budgets are granted and enforced

Found by QA-009 (P1, cost controls): the served product enforced no cost control. RUN-010's
`BudgetService` (grants, child ≤ parent, charges, exhaustion) and OPS-004's `usage::quota` existed as
libraries with no production caller: the engine enforced only a run's step budget, nothing charged a
model call's tokens or a tool call to any budget, no command could grant a budget (and budget ids had
no canonical prefix to be minted with), and a spent budget could not stop anything. `charge` was also
not atomic: a load, a check and a store in separate transactions, so concurrent turns could both pass a
limit and lose an update.

Fix:
- Identity: `bdg_` (`Budget`) joins the canonical prefixes (DOMAIN.md §1.1, `identity.proto`
  `ENTITY_PREFIX_BUDGET`, `quansio_core::Prefix`), regenerated catalogs, OpenAPI and SDK bindings.
- `SetBudget {scope: tenant|workspace, workspace_id?, limits}` (DOMAIN.md §9 Admin/ops, `ManageTenant` /
  `ManageWorkspace`): grants the tenant's or a workspace's budget, nested under the tenant budget when
  there is one, or re-limits it keeping what it consumed (`BudgetService::set_limits`, still within the
  parent's remaining). `GET /v1/usage` lists the budgets.
- Enforcement (`runtime/budgets/enforce.rs`, `BudgetPort` in the engine): a run is governed by its agent
  thread's, workspace's and tenant's budgets. Before each model call and each tool round the engine
  checks none is exhausted — otherwise the turn stops and the run is suspended with `BUDGET_EXHAUSTED`,
  as for the step budget — and after each model call it charges the tokens the provider reported
  (`ModelProposal::usage`, from the gateway), after each round the tool calls.
- `BudgetService::record` charges what work already done used, atomically under a row lock (the charge
  applies whatever the limit — the work happened — and a budget taken to or past a limit becomes
  exhausted), emits `usage.recorded`, `usage.exhausted` once, and `usage.threshold_crossed` once per
  meter when a charge takes it past 80 % of its limit: the soft quota (OPS-004); the limits are the hard
  quota. DOMAIN.md §13.2 states the rules.

Tests (`tests.log`): `set_budget_grants_and_relimits_within_the_tenant_budget` (tenant grant with a
`bdg_` id, a nested workspace grant, a request beyond the parent refused `VALIDATION_BOUNDS`, a re-limit
keeps the budget, a viewer refused `SCOPE_FORBIDDEN`, an unknown scope refused, both listed by
`/v1/usage`); `a_spent_budget_starts_no_new_model_call` (a 50-token workspace budget, a model that
reports 20 tokens a call: three turns charge 60 and exhaust it, the fourth starts no model call and the
run is suspended, the threshold and exhaustion are announced once each, raising the limit lets work
start again); `concurrent_charges_add_up` (twenty concurrent charges of 7 add to 140); the budgets, usage,
state machine, gateway skeleton and conversation suites; fmt and clippy (`lint.log`); contract drift
(`contracts.log`); the Rust, Python and TypeScript SDK suites over the regenerated bindings (`sdk.log`). Mutations (`mutation.log`): the engine ignoring a spent budget, a model call not charged, and the soft quota announced on every charge — each fails the enforcement test after a passing control.
