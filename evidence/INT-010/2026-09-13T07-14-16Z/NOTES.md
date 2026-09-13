# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## unit

4 — the metric implementations. This closes INT-010's implementation.

## acceptance

- **Evaluation runs are reproducible from pinned inputs.** A dataset is content-addressed and a drifted
  file is refused at load (unit 1), a run pins the datasets it used with the digest each was loaded at
  and is itself content-addressed over its pins, versions and outcomes, and `comparable_with` names the
  input or version that differs (units 2 and 4).
- **Protected safety/recovery regressions block promotion regardless of aggregate quality gain.** The
  gate evaluates protected metrics first and sets `promotion_blocked` on a protected failure while
  reporting every quality metric (unit 3), and unit 4 makes the row *countable*: metrics report per-case
  outcomes, runs record them, and `protected_regressions` counts exactly the protected cases that passed
  in a baseline and fail in a candidate - a new protected case is reported, not counted, so the number
  means one thing.

## recorded_decisions

- **Per-case outcomes exist because of one row.** DOSSIER 21.3's last entry is a statement about
  individual protected cases across runs; an aggregate cannot express it, so the metrics return case
  outcomes and the run keeps them (and hashes them).
- **Each metric observes something the plane can see.** Route quality resolves through the *configured*
  selector, which the test proves by scoring 1.0 with a class-driven selector and below 1.0 with the
  class-blind default - a metric that could not distinguish those two deployments would be restating the
  dataset rather than measuring routing.
- **Two metrics are ports, not inventions.** Answer grounding needs a verifier (RUN-008's seam) and
  tool-proposal validity needs the tool registry's own schema check (RUN-011, Rust). Re-implementing that
  schema subset in Python would be a second, drifting authority, so with no implementation installed the
  metric is reported as not measured - and the gate fails closed on an unmeasured thresholded metric, so
  the absence cannot become a silent pass.
- **The injection count is a precondition, not an effect execution.** This plane executes no effects, so
  "unauthorized effect executions" measures that no malicious sample reaches context as an instruction
  rather than as data. The effect-level check stays the runtime's (INT-012/RUN-006 verify it there), and
  saying so is more honest than implying this harness proves anything about effect execution.
- **Skill resolution is measured but not gated.** 21.3 sets no threshold for it, so the measurement is
  reported and the gate lists it as ungated rather than the harness inventing a bar - an unrequired
  threshold would be a second authority for what "good" means.
- **Retrieval is measured where a real index exists.** `measure_retrieval` drives an `EmbeddingIndex` and
  a second tenant's index: recall@10 against the caller's pinned expectations, no foreign row, and no
  row for a source removed with `delete_source`. The database-backed evaluation run belongs to the
  qualification path that has a store and an embedder (QA-004/QA-003), and the function is written so it
  measures rather than assumes.
