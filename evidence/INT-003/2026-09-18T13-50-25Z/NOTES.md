# INT-003 — real-boundary proof of routing, DLP and failover; guard clearance fix

## Why now

INT-003 is `real_boundary: true`; its blocker deferred "the routing, DLP and failover suites
against a live provider" to INT-002's boundary, which now exists (Z.ai: the OpenAI-compatible
adapter on `api.z.ai/api/coding/paas/v4`, and the Anthropic adapter on `api.z.ai/api/anthropic`,
both serving GLM-5.3-Flash). The offline suites prove the policy against the stub; this bundle
proves the three named tests against a real provider.

## What was run (`python/tests/intelligence/test_model_routing_live.py`, new)

The real transport is wrapped only to record the URL of every connection it opens; nothing is
mocked. Each case ran on both boundaries (`live-routing-openai-compatible-zai.log`,
`live-routing-anthropic-wire-zai.log`): 3 passed each. Credentials came from the
`QUANSIO_TEST_*` variables and appear nowhere in this bundle (scanned).

- **Route determinism / no auxiliary call (acceptance 1).** Resolving the route twice gives
  the same model, route id and `chosen_by` rule with zero connections opened; fulfilling then
  opens exactly one connection, to the selected route's provider, and the outcome is terminal
  `END_TURN` with real usage.
- **DLP denial before transmission (acceptance 2).** A request at a protected class
  (`dlp_profile=strict` → confidential) raises `DLP_DENIED` with zero connections opened; the
  same content at the default class transmits (one connection), so the denial was policy.
- **Provider failover.** A catalog whose primary is a first-party route at a refused loopback
  port and whose only fallback is the live route: `bounded_failover` leaves the primary after
  its retryable `PROVIDER_UNAVAILABLE`, the live fallback answers the *same* `call_id`, and
  exactly two connections were opened (primary, then fallback) — bounded, no retry storm.

## Defect the boundary exposed (fixed in this task's own path)

`DlpGuard.check` refused a `dlp_eligible: false` model for every data class. `config/models.yaml`
defines the flag as whether the model may receive content classified *above* the default
data-handling class, and the selector explicitly defers clearance to the guard — so with the
guard installed, a self-hosted route (the committed `openai-compatible-local`) could never be
called at all, not even for public content. The first live attempt at the default class was
refused with `dlp.model_eligibility`. The model rule now fires only for protected classes
(`DlpPolicy.is_protected`: above the policy's default); the provider rule is unchanged.
`test_the_guard_refuses_a_model_that_is_not_cleared` now covers both sides.

## Gates on the merge commit (9282089)

- routing + DLP + live suites (live unconfigured here: 3 `BLOCKED_EXTERNAL` skips) — 6 passed
  (`routing-dlp-suites.log`).
- `(cd python && uv run --frozen pytest -q)` with `QUANSIO_TEST_POSTGRES_URL` — 518 passed,
  13 skipped (`pytest-python.log`).
- ruff / ruff format / mypy — clean (`lint-type.log`).
- `python3.12 scripts/validate_v81.py` — PASS (`validate.log`).
- `bash scripts/ci/ci.sh` — PASS (`ci.log`).
