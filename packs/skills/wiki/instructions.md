# WikiSkill

Use existing primitives only (DOMAIN.md §11.3–§11.4, INT-005/006/009):

1. Build a typed `SearchProgram` (`lexical` / `exact` channels, closed predicates).
2. Retrieve only `active` Knowledge Fabric entries; skip quarantined/deleted.
3. Preserve provenance on every hit; do not drop source identity when summarizing.
4. Pack hits into a `ContextProjection` under the token budget; record degradation.
5. Cite with `knowledge.cite`. Never grant a tool the caller does not hold.
