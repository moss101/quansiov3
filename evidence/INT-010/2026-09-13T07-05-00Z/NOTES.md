# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the
summary; a bundle may carry other evidence files).

## unit

1 — the pinned, versioned datasets and the protected gate configuration. Units 2 (the run record with
cost/latency capture), 3 (the decision gate) and 4 (the metric implementations) remain.

## recorded_decisions

- A dataset is **content-addressed**: its digest covers the identity, the version, the kind and every
  case in order, and nothing else — so a change to a payload, to case order or to the version is a
  different dataset, while editing prose in `notes` is not. Load refuses a file whose recorded digest
  does not match its content, which is what makes "reproducible from pinned inputs" a property of the
  shipped loader rather than a promise in a document.
- A case's `payload` stays **opaque** to this module: interpreting it is the metric's job, and keeping
  it opaque is what stops the harness from inventing case semantics the dataset never stated.
- The gate configuration encodes DOSSIER §21.3 **verbatim**, and the test parses the table out of the
  document and compares metric by metric, including the comparison directions. One normalization is
  recorded rather than hidden: §21.3 writes ratio metrics as percentages ("100 %", "≥ 98 %") and the
  configuration stores them as 0..1 values, so the test divides by 100. A row with no direction symbol
  means exactly that figure — 100 % is perfect, 0 is zero — and the test refuses an ambiguous row
  rather than guessing a direction for it.
- The metric *names* are this configuration's; the table's row labels are mapped explicitly in one
  place (`ROW_TO_METRIC`) so a rename is visible, and the set of names is asserted to cover the table
  exactly: an unencoded metric would silently not be gated, and an extra one would gate something the
  authority does not require.
- `protected: true` marks the five metrics whose violation is a safety or recovery failure rather than
  a quality dip (route determinism, cross-tenant retrieval, deleted-memory retrieval after refresh,
  injection unauthorized effects, protected recovery/safety regressions), and `blocks_promotion` marks
  the row §21.3 itself calls blocking. The gate that consumes this configuration is unit 3; the flag
  existing here means the configuration already states what is not tradeable.
- `ratified: false` and the `source` field record that §21.3 itself says owner ratification is
  required — a gate built on this file is a measurement until the owner ratifies it.
- **The supply-chain gate caught the dataset file names.** The OPS-007 quarantine rule reads a `skill`
  or `tool` token in a *file name* as an imported-manifest marker anywhere in the tree, and the two
  datasets were first named `tool_proposal.v1.json` and `skill_behaviour.v1.json` after their
  families. A dataset is not an imported manifest, so the files and their dataset ids were renamed
  (`proposal_validity`, `resolution_behaviour`) while the family stays stated in the `kind` field —
  the same shape as the earlier fix for the architecture gate's authority-write rule: the gate is
  authoritative and the artifact moves, rather than the rule being narrowed.
- The injection metrics read INT-012's corpus at `tests/security/injection/corpus.json` and the
  thresholds file references it; a test asserts no copy of it exists under the datasets directory, so
  the corpus keeps one authority.
