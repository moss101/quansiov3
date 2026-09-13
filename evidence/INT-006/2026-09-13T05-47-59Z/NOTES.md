# Evidence notes

Narrative that the canonical `summary.json` schema does not carry.

## unit

2

## real_boundary_evidence



## note

INT-006 unit 2: the durable store over public.knowledge_entries. The database-backed suite is the release evidence; the recording-store suite exists to prove what a database-backed suite cannot, namely that a refused call reaches no I/O. The task is not complete: unit 3 (ingestion from approved sources and verified run outcomes, calling the INT-011 deletion seam) and unit 4 (the semantic channel and the evidence bundle) remain, and INT-006 cannot reach PASS while its dependency INT-011 is BLOCKED_EXTERNAL.

## recorded_decisions

- public.knowledge_entries is the authority: no second knowledge table and no second semantic structure was created, and the derived channel stays INT-011's.
- The tenant is intrinsic to the fabric (bound at construction, no method accepts one, every statement filtered) and the database's own row-level security WITH CHECK is the second check, verified by writing through the application role with a missing context.
- Lifecycle moves go through the model's with_status and every UPDATE is guarded on the state the caller read, so an illegal edge and a concurrent move are both refused instead of being applied because SQL can.
- A batch of moves is one transaction, so a source deletion is never half applied.
- delete is the model's terminal lifecycle edge and the row is retained, so provenance, audit and a future replacement survive; quarantine is never replaced by deletion.
- The derived pointer (embedding_ref) is refused by this store because it belongs to the vector index (INT-011); persisting it here would make this store a second derived authority.
- Inline content JSONB is left unset: neither DOMAIN.md 11.4 nor the generated KnowledgeEntry contract names an inline payload field, so adding one is additive work for the task that needs it rather than a field invented here.
- Timestamps are the database's (created_at/updated_at defaults): neither DOMAIN.md 11.4 nor the generated contract exposes them, so the store never writes them, and a test proves a status change leaves created_at alone.
- knowledge_entries.id is the primary key, so identities are globally unique (a real deployment mints ULIDs); the isolation test gives each tenant its own identity.
