# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## unit

2 — the run record. Unit 3 (the decision gate) and unit 4 (the metric implementations) remain.

## recorded_decisions

- A run is content-addressed over **exactly** its pins, versions and measurements. Cost and latency are
  deliberately excluded: they describe the environment the run happened in, not what was measured, so
  including them would make two runs of the same inputs look incomparable for an unrelated reason.
- `comparable_with` returns *why* two runs are not comparable (the differing pin or version) rather than
  a bare boolean, because the reason is what an operator acts on; the acceptance statement is that a run
  is reproducible from pinned inputs, and a diff that names the moved input is how that is checked.
- Skill versions are compared only when the caller asks. A routing or retrieval run does not depend on
  which skill versions exist, so treating them as part of comparability by default would refuse
  comparisons that are perfectly meaningful.
- Measurements are validated in the units the gate speaks (ratio in 0..1, count non-negative, finite),
  and a metric that was not measured is simply absent: the gate fails closed on a missing measurement
  rather than reading a default, which is the difference between "not measured" and "measured zero".
- Loading a record validates every field instead of coercing, and refuses a record whose content does
  not hash to its digest - a record read from disk is data, and malformed data must not become a run
  that looks measured but is not.
