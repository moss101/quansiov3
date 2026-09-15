# Kill switches (OPS-008)

Not authority — DOSSIER.md §21.5 is. Each control is `runtime.control`, audited, reversible.

1. `provider.disable` — stop new model calls to a provider. Release with `EnableProvider`.
2. `connector.revoke` — stop connector dispatch. Existing evidence stays.
3. `worker.quarantine` — fence one qworkerd. Other workers continue.
4. `target.drain` — no new leases; in-flight work finishes or is fenced.
5. `effect.freeze` — block new tier ≥ 2 effects. Reads and evidence continue.
6. `policy.emergency_deny` — fail-closed on matching effect classes.
7. `stream.shed` — disconnect slow `/v1/stream` consumers.

Engage and release are written to the audit chain. Do not delete audit rows.
