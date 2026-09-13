"""The decision gate: protected metrics first, and they block (INT-010 unit 3, DOSSIER §21).

The gate answers one question — may this change be promoted? — and it answers it in the order the
authority states it. DOSSIER §21.3's last row is *protected recovery/safety regressions: 0 (blocks
promotion)*, so a protected metric is not one more number in a weighted score:

* **protected metrics are evaluated and reported first**, and a protected failure sets
  `promotion_blocked` whatever the quality metrics say. A quality improvement cannot buy off a safety or
  recovery regression, which is the task's second acceptance statement made executable;
* **a gate is a gate.** A quality metric outside its threshold still fails the verdict — it does not
  block promotion the way a protected one does, but a run that misses a stated bar has not passed it,
  and saying otherwise would turn the configuration into decoration;
* **missing data fails closed.** A thresholded metric the run did not measure is a failure, not a skip:
  "unmeasured" and "measured at zero" are different facts, and only one of them is evidence;
* **the verdict records the configuration it applied**, so a decision names the thresholds version, the
  document it came from and whether those thresholds are ratified — a provisional gate must be visible
  as provisional.

The gate measures and decides; it never promotes. Promotion is the operator's and REL-001's, so nothing
here mutates a registry, a configuration or product state.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

import yaml

from intelligence.evaluation.runs import EvaluationRun, Measurement

#: Refusal rules, named so a caller can tell which one fired.
RULE_THRESHOLD_SHAPE = "thresholds.shape"
RULE_THRESHOLD_METRIC = "thresholds.metric"
RULE_THRESHOLD_COMPARISON = "thresholds.comparison"
RULE_THRESHOLD_IO = "thresholds.io"

#: The comparison directions a threshold may state, and the units a metric may carry.
COMPARISONS: tuple[str, ...] = ("at_least", "at_most")
UNITS: tuple[str, ...] = ("ratio", "count")


class ThresholdError(ValueError):
    """A refused gate configuration, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class GateThreshold:
    """One configured metric: how it must compare, what it guards, and whether it blocks promotion."""

    metric: str
    comparison: str
    threshold: float
    unit: str
    description: str = ""
    protected: bool = False
    blocks_promotion: bool = False

    def __post_init__(self) -> None:
        if not self.metric.strip():
            raise ThresholdError("VALIDATION_SCHEMA", RULE_THRESHOLD_METRIC, "a threshold needs a metric")
        if self.comparison not in COMPARISONS:
            raise ThresholdError(
                "VALIDATION_SCHEMA",
                RULE_THRESHOLD_COMPARISON,
                f"{self.metric}: comparison {self.comparison!r} is not one of {list(COMPARISONS)}",
            )
        if self.unit not in UNITS:
            raise ThresholdError(
                "VALIDATION_SCHEMA",
                RULE_THRESHOLD_COMPARISON,
                f"{self.metric}: unit {self.unit!r} is not one of {list(UNITS)}",
            )
        if self.blocks_promotion and not self.protected:
            raise ThresholdError(
                "VALIDATION_SCHEMA",
                RULE_THRESHOLD_SHAPE,
                f"{self.metric}: only a protected metric may block promotion",
            )

    def holds(self, value: float) -> bool:
        """Whether a measured value satisfies this threshold."""
        if self.comparison == "at_least":
            return value >= self.threshold
        return value <= self.threshold

    def describe(self) -> str:
        symbol = ">=" if self.comparison == "at_least" else "<="
        return f"{self.metric} {symbol} {self.threshold} ({self.unit})"


@dataclass(frozen=True, slots=True)
class ThresholdSet:
    """The gate configuration: its version, its source, and one threshold per metric."""

    version: int
    ratified: bool
    source: str
    thresholds: Mapping[str, GateThreshold]

    def __post_init__(self) -> None:
        if self.version < 1:
            raise ThresholdError(
                "VALIDATION_SCHEMA", RULE_THRESHOLD_SHAPE, f"version {self.version} must be >= 1"
            )
        if not self.thresholds:
            raise ThresholdError(
                "VALIDATION_SCHEMA", RULE_THRESHOLD_SHAPE, "a gate with no thresholds gates nothing"
            )
        if not self.source.strip():
            raise ThresholdError(
                "VALIDATION_SCHEMA", RULE_THRESHOLD_SHAPE, "a gate must name the authority it encodes"
            )

    @property
    def protected(self) -> tuple[GateThreshold, ...]:
        """The protected metrics, in configuration order."""
        return tuple(item for item in self.thresholds.values() if item.protected)

    def __getitem__(self, metric: str) -> GateThreshold:
        item = self.thresholds.get(metric)
        if item is None:
            raise ThresholdError("NOT_FOUND", RULE_THRESHOLD_METRIC, f"no threshold for {metric!r}")
        return item

    @classmethod
    def from_mapping(cls, data: object, *, source: str = "") -> ThresholdSet:
        """Build a threshold set from decoded configuration, refusing an unknown or malformed shape."""
        if not isinstance(data, Mapping):
            raise ThresholdError(
                "VALIDATION_SCHEMA", RULE_THRESHOLD_IO, f"{source or 'configuration'} is not an object"
            )
        raw_metrics = data.get("metrics")
        if not isinstance(raw_metrics, Mapping) or not raw_metrics:
            raise ThresholdError(
                "VALIDATION_SCHEMA", RULE_THRESHOLD_SHAPE, f"{source or 'configuration'} declares no metrics"
            )
        thresholds: dict[str, GateThreshold] = {}
        for metric, entry in raw_metrics.items():
            if not isinstance(entry, Mapping):
                raise ThresholdError(
                    "VALIDATION_SCHEMA",
                    RULE_THRESHOLD_METRIC,
                    f"{metric!r} is not a threshold object",
                )
            value = entry.get("threshold")
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise ThresholdError(
                    "VALIDATION_SCHEMA",
                    RULE_THRESHOLD_METRIC,
                    f"{metric!r} carries no numeric threshold",
                )
            thresholds[str(metric)] = GateThreshold(
                metric=str(metric),
                comparison=str(entry.get("comparison", "")),
                threshold=float(value),
                unit=str(entry.get("unit", "")),
                description=str(entry.get("description", "")),
                protected=bool(entry.get("protected", False)),
                blocks_promotion=bool(entry.get("blocks_promotion", False)),
            )
        return cls(
            version=int(data.get("version", 0)),
            ratified=bool(data.get("ratified", False)),
            source=str(data.get("source", source)),
            thresholds=thresholds,
        )

    @classmethod
    def load(cls, path: Path) -> ThresholdSet:
        """Read the configuration, refusing an unreadable or malformed file."""
        try:
            decoded = yaml.safe_load(path.read_text(encoding="utf-8"))
        except OSError as error:
            raise ThresholdError("NOT_FOUND", RULE_THRESHOLD_IO, f"{path} is unreadable") from error
        except yaml.YAMLError as error:
            raise ThresholdError(
                "VALIDATION_SCHEMA", RULE_THRESHOLD_IO, f"{path} is not valid YAML"
            ) from error
        return cls.from_mapping(decoded, source=str(path))


@dataclass(frozen=True, slots=True)
class MetricVerdict:
    """One metric's outcome: what it was measured at against what it must hold."""

    metric: str
    protected: bool
    measured: bool
    value: float | None
    threshold: float
    comparison: str
    passed: bool
    detail: str

    def as_mapping(self) -> dict[str, object]:
        return {
            "metric": self.metric,
            "protected": self.protected,
            "measured": self.measured,
            "value": self.value,
            "threshold": self.threshold,
            "comparison": self.comparison,
            "passed": self.passed,
            "detail": self.detail,
        }


@dataclass(frozen=True, slots=True)
class GateVerdict:
    """The decision, with everything it was made from."""

    passed: bool
    promotion_blocked: bool
    verdicts: tuple[MetricVerdict, ...]
    blockers: tuple[MetricVerdict, ...]
    failures: tuple[MetricVerdict, ...]
    ungated: tuple[str, ...]
    thresholds_version: int
    ratified: bool
    source: str
    reason: str

    def as_mapping(self) -> dict[str, object]:
        return {
            "passed": self.passed,
            "promotion_blocked": self.promotion_blocked,
            "reason": self.reason,
            "blockers": [item.as_mapping() for item in self.blockers],
            "failures": [item.as_mapping() for item in self.failures],
            "verdicts": [item.as_mapping() for item in self.verdicts],
            "ungated": list(self.ungated),
            "thresholds": {
                "version": self.thresholds_version,
                "ratified": self.ratified,
                "source": self.source,
            },
        }


def evaluate_gate(run: EvaluationRun, thresholds: ThresholdSet) -> GateVerdict:
    """Decide whether a run may be promoted, protected metrics first.

    Every configured metric gets a verdict, in configuration order, with the protected ones first; a
    protected failure blocks promotion whatever the rest of the run says, and a metric the run did not
    measure fails closed.
    """
    verdicts: list[MetricVerdict] = []
    ordered: Sequence[GateThreshold] = sorted(
        thresholds.thresholds.values(), key=lambda item: (not item.protected, item.metric)
    )
    measured = {item.metric for item in run.measurements}
    for threshold in ordered:
        measurement = run.measurement(threshold.metric)
        if measurement is None:
            verdicts.append(
                MetricVerdict(
                    metric=threshold.metric,
                    protected=threshold.protected,
                    measured=False,
                    value=None,
                    threshold=threshold.threshold,
                    comparison=threshold.comparison,
                    passed=False,
                    detail=f"not measured by this run; {threshold.describe()} was not checked",
                )
            )
            continue
        verdicts.append(_verdict_for(threshold, measurement))

    blockers = tuple(item for item in verdicts if item.protected and not item.passed)
    failures = tuple(item for item in verdicts if not item.passed and not item.protected)
    ungated = tuple(sorted(measured - set(thresholds.thresholds)))
    blocking_required = tuple(
        item for item in verdicts if item.protected and not item.passed and _blocks(thresholds, item.metric)
    )
    promotion_blocked = bool(blockers) or bool(blocking_required)
    passed = not blockers and not failures

    if promotion_blocked:
        names = ", ".join(item.metric for item in blockers)
        reason = (
            f"promotion blocked: protected metric(s) {names} did not hold "
            f"({thresholds.source}); a quality gain cannot trade against them"
        )
    elif passed:
        reason = f"every configured metric holds ({thresholds.source})"
    else:
        names = ", ".join(item.metric for item in failures)
        reason = f"quality metric(s) {names} did not hold ({thresholds.source})"

    return GateVerdict(
        passed=passed,
        promotion_blocked=promotion_blocked,
        verdicts=tuple(verdicts),
        blockers=blockers,
        failures=failures,
        ungated=ungated,
        thresholds_version=thresholds.version,
        ratified=thresholds.ratified,
        source=thresholds.source,
        reason=reason,
    )


def _blocks(thresholds: ThresholdSet, metric: str) -> bool:
    item = thresholds.thresholds.get(metric)
    return bool(item is not None and item.blocks_promotion)


def _verdict_for(threshold: GateThreshold, measurement: Measurement) -> MetricVerdict:
    if measurement.unit != threshold.unit:
        return MetricVerdict(
            metric=threshold.metric,
            protected=threshold.protected,
            measured=True,
            value=float(measurement.value),
            threshold=threshold.threshold,
            comparison=threshold.comparison,
            passed=False,
            detail=(
                f"measured as {measurement.unit}, configured as {threshold.unit}: the comparison "
                "would be meaningless, so it fails closed"
            ),
        )
    held = threshold.holds(float(measurement.value))
    if held:
        detail = f"measured {measurement.value} against {threshold.describe()}"
    else:
        detail = f"measured {measurement.value}, below {threshold.describe()}"
        if threshold.comparison == "at_most":
            detail = f"measured {measurement.value}, above {threshold.describe()}"
    return MetricVerdict(
        metric=threshold.metric,
        protected=threshold.protected,
        measured=True,
        value=float(measurement.value),
        threshold=threshold.threshold,
        comparison=threshold.comparison,
        passed=held,
        detail=detail,
    )
