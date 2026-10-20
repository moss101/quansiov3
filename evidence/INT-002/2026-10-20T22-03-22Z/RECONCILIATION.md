# INT-002 — see evidence/APP-007/2026-10-20T22-03-22Z/RECONCILIATION.md

This task's change was made in the same branch as the APP-007 live-surface work and is proved by the same gate; this bundle
names which of its logs and tests prove INT-002. Proof for INT-002: `test_model_gateway_pictures.py` (optional pictures; Anthropic and OpenAI carriage; a model without vision is told; a required picture still fails; a fixed token cost). The worker and server halves that produce and attach a screenshot are a later change.
