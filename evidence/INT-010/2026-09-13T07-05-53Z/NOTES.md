# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## unit

3 — the decision gate. Unit 4 (the metric implementations that produce a run's measurements) remains.

## recorded_decisions

- **A protected metric is not one more number.** DOSSIER §21.3's last row says protected recovery/safety
  regressions block promotion, so the gate evaluates and reports the protected metrics first and sets
  `promotion_blocked` on a protected failure whatever the quality metrics say. The acceptance test
  states it directly: every quality metric at its best and one leaked tenant row, and promotion is
  blocked because a quality gain cannot be traded against a safety failure. `blockers` and `failures`
  are reported separately for the same reason - a reader must be able to see which kind of miss they are
  looking at.
- **A gate is a gate.** A quality metric outside its threshold still fails the verdict; it simply does
  not block promotion the way a protected one does. The alternative - treating a quality miss as
  informational - would turn the configuration into decoration.
- **Missing data fails closed, and so does a mis-unit measurement.** A thresholded metric the run did
  not measure is a failure (the test that proves it measures one metric and asserts the remaining eight
  fail), and a count offered against a ratio threshold fails rather than being compared, because the
  comparison would be meaningless.
- **An ungated measurement is reported, not ignored.** A run may measure more than the configuration
  gates; those names come back in `ungated` so a reader sees what was measured and deliberately not
  gated, rather than having it silently dropped.
- **The verdict records the configuration that decided it** - the version, the authority it came from
  and whether it is ratified. §21.3 is provisional pending owner ratification, and a decision must not
  be able to look like it was made under a ratified bar when it was not.
- The gate measures and decides; it mutates nothing. Promotion stays the operator's and REL-001's.
