"""The record of one evaluation run (INT-010 unit 2, DOSSIER §21).

A measurement is only evidence if you can say what it measured *and* what produced it. A run record is
that statement, and it is deliberately narrow: the pinned inputs (each dataset with the digest it was
loaded at), the implementation versions the run was produced under (model route, embedding route,
index snapshot, skill versions), the measurements themselves, and what the run cost and how long it
took.

Two consequences are enforced rather than described:

* **A run is content-addressed.** Its digest covers the pins, the versions and the measurements, so two
  runs of the same inputs and versions are comparable, and a run whose input moved is a different run
  rather than a quietly inconsistent measurement;
* **Comparability is a check, not an assumption.** [`EvaluationRun.comparable_with`] answers whether
  two runs measured the same thing, and names the pin or version that differs when they did not. Cost
  and latency are *not* part of comparability — they are observations about the environment, not about
  what was measured — but they are part of the record, read against the provisional SLOs in
  DOSSIER §21.2.
"""

from __future__ import annotations

import hashlib
import json
import math
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

from intelligence.evaluation.datasets import Dataset

#: Refusal rules, named so a caller can tell which one fired.
RULE_RUN_ID = "run.id"
RULE_RUN_PINS = "run.pins"
RULE_RUN_MEASUREMENT = "run.measurement"
RULE_RUN_VERSIONS = "run.versions"
RULE_RUN_COST = "run.cost"
RULE_RUN_SHAPE = "run.shape"

#: The units a measurement may carry; the thresholds configuration uses the same two.
UNITS: tuple[str, ...] = ("ratio", "count")


class RunError(ValueError):
    """A refused run record or measurement, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class DatasetPin:
    """One dataset as a run used it: identity, version and the digest it was loaded at."""

    dataset_id: str
    version: int
    digest: str
    cases: int = 0

    def __post_init__(self) -> None:
        if not self.dataset_id.strip():
            raise RunError("VALIDATION_SCHEMA", RULE_RUN_PINS, "a pin needs a dataset identity")
        if self.version < 1:
            raise RunError("VALIDATION_SCHEMA", RULE_RUN_PINS, f"pin version {self.version} must be >= 1")
        if len(self.digest) != 64:
            raise RunError(
                "VALIDATION_SCHEMA",
                RULE_RUN_PINS,
                f"pin {self.dataset_id!r} carries digest {self.digest!r}, which is not a sha256",
            )

    @classmethod
    def of(cls, dataset: Dataset) -> DatasetPin:
        """Pin a loaded dataset, so a run names the exact inputs it measured against."""
        return cls(
            dataset_id=dataset.dataset_id,
            version=dataset.version,
            digest=dataset.digest,
            cases=len(dataset.cases),
        )

    def as_mapping(self) -> dict[str, object]:
        return {
            "dataset_id": self.dataset_id,
            "version": self.version,
            "digest": self.digest,
            "cases": self.cases,
        }


@dataclass(frozen=True, slots=True)
class ImplementationVersions:
    """The versions a run was produced under; a change in any of them is a different run."""

    model_route: str = ""
    provider: str = ""
    embedding_route: str = ""
    index_snapshot: str = ""
    skill_versions: tuple[tuple[str, str], ...] = ()
    harness_version: str = "v1"

    def __post_init__(self) -> None:
        for name, value in (
            ("model_route", self.model_route),
            ("provider", self.provider),
            ("embedding_route", self.embedding_route),
            ("index_snapshot", self.index_snapshot),
            ("harness_version", self.harness_version),
        ):
            if value and not value.strip():
                raise RunError("VALIDATION_SCHEMA", RULE_RUN_VERSIONS, f"{name} is blank")
        seen: set[str] = set()
        for skill_id, version in self.skill_versions:
            if not skill_id.strip() or not version.strip():
                raise RunError(
                    "VALIDATION_SCHEMA", RULE_RUN_VERSIONS, "a skill version needs both its skill and version"
                )
            if skill_id in seen:
                raise RunError(
                    "CONFLICT_STATE", RULE_RUN_VERSIONS, f"skill {skill_id!r} appears twice in the versions"
                )
            seen.add(skill_id)

    def as_mapping(self) -> dict[str, object]:
        return {
            "model_route": self.model_route,
            "provider": self.provider,
            "embedding_route": self.embedding_route,
            "index_snapshot": self.index_snapshot,
            "skill_versions": [list(item) for item in self.skill_versions],
            "harness_version": self.harness_version,
        }


@dataclass(frozen=True, slots=True)
class Measurement:
    """One metric's measured value, in the unit the thresholds configuration uses."""

    metric: str
    value: float
    unit: str
    cases: int = 0
    notes: str = ""

    def __post_init__(self) -> None:
        if not self.metric.strip():
            raise RunError("VALIDATION_SCHEMA", RULE_RUN_MEASUREMENT, "a measurement needs its metric")
        if not isinstance(self.value, (int, float)) or not math.isfinite(self.value):
            raise RunError(
                "VALIDATION_SCHEMA",
                RULE_RUN_MEASUREMENT,
                f"{self.metric}: value {self.value!r} is not a finite number",
            )
        if self.unit not in UNITS:
            raise RunError(
                "VALIDATION_SCHEMA",
                RULE_RUN_MEASUREMENT,
                f"{self.metric}: unit {self.unit!r} is not one of {list(UNITS)}",
            )
        if self.unit == "ratio" and not 0.0 <= float(self.value) <= 1.0:
            raise RunError(
                "VALIDATION_SCHEMA",
                RULE_RUN_MEASUREMENT,
                f"{self.metric}: ratio {self.value!r} is outside 0..1",
            )
        if self.unit == "count" and float(self.value) < 0:
            raise RunError(
                "VALIDATION_SCHEMA", RULE_RUN_MEASUREMENT, f"{self.metric}: count {self.value!r} is negative"
            )
        if self.cases < 0:
            raise RunError(
                "VALIDATION_SCHEMA", RULE_RUN_MEASUREMENT, f"{self.metric}: case count is negative"
            )

    def as_mapping(self) -> dict[str, object]:
        return {
            "metric": self.metric,
            "value": float(self.value),
            "unit": self.unit,
            "cases": self.cases,
            "notes": self.notes,
        }


@dataclass(frozen=True, slots=True)
class CostLatency:
    """What the run cost and how long it took; read against the provisional SLOs in DOSSIER §21.2."""

    wall_time_ms: int
    model_calls: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    cost_minor_units: int = 0
    slowest_index_query_ms: int = 0

    def __post_init__(self) -> None:
        for name, value in (
            ("wall_time_ms", self.wall_time_ms),
            ("model_calls", self.model_calls),
            ("input_tokens", self.input_tokens),
            ("output_tokens", self.output_tokens),
            ("cost_minor_units", self.cost_minor_units),
            ("slowest_index_query_ms", self.slowest_index_query_ms),
        ):
            if value < 0:
                raise RunError("VALIDATION_SCHEMA", RULE_RUN_COST, f"{name} is negative ({value})")

    def as_mapping(self) -> dict[str, object]:
        return {
            "wall_time_ms": self.wall_time_ms,
            "model_calls": self.model_calls,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cost_minor_units": self.cost_minor_units,
            "slowest_index_query_ms": self.slowest_index_query_ms,
        }


@dataclass(frozen=True, slots=True)
class EvaluationRun:
    """One evaluation run: what it measured, under which versions, at what cost."""

    run_id: str
    pins: tuple[DatasetPin, ...]
    measurements: tuple[Measurement, ...]
    versions: ImplementationVersions
    cost: CostLatency
    started_at: str = ""
    finished_at: str = ""

    def __post_init__(self) -> None:
        if not self.run_id.strip():
            raise RunError("VALIDATION_SCHEMA", RULE_RUN_ID, "a run needs an identity")
        if not self.pins:
            raise RunError(
                "VALIDATION_SCHEMA",
                RULE_RUN_PINS,
                "a run with no pinned dataset measured nothing that can be reproduced",
            )
        pinned: set[str] = set()
        for pin in self.pins:
            if pin.dataset_id in pinned:
                raise RunError(
                    "CONFLICT_STATE",
                    RULE_RUN_PINS,
                    f"dataset {pin.dataset_id!r} is pinned twice in one run",
                )
            pinned.add(pin.dataset_id)
        seen: set[str] = set()
        for measurement in self.measurements:
            if measurement.metric in seen:
                raise RunError(
                    "CONFLICT_STATE",
                    RULE_RUN_MEASUREMENT,
                    f"metric {measurement.metric!r} is measured twice in one run",
                )
            seen.add(measurement.metric)
        if self.started_at and self.finished_at and self.finished_at < self.started_at:
            raise RunError("VALIDATION_SCHEMA", RULE_RUN_SHAPE, "the run finished before it started")

    # -- lookups -------------------------------------------------------------------

    def pin(self, dataset_id: str) -> DatasetPin:
        for item in self.pins:
            if item.dataset_id == dataset_id:
                return item
        raise RunError("NOT_FOUND", RULE_RUN_PINS, f"the run pinned no dataset {dataset_id!r}")

    def measurement(self, metric: str) -> Measurement | None:
        for item in self.measurements:
            if item.metric == metric:
                return item
        return None

    # -- identity ------------------------------------------------------------------

    @property
    def digest(self) -> str:
        """The content address of the run: its pins, versions and measurements, and nothing else."""
        return hashlib.sha256(self.canonical_bytes()).hexdigest()

    def canonical_bytes(self) -> bytes:
        return json.dumps(
            {
                "pins": [pin.as_mapping() for pin in self.pins],
                "measurements": [item.as_mapping() for item in self.measurements],
                "versions": self.versions.as_mapping(),
            },
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")

    def comparable_with(self, other: EvaluationRun, *, skills: bool = False) -> tuple[bool, str]:
        """Whether two runs measured the same thing, and what differs when they did not.

        The comparison is over the pinned inputs and the implementation versions — the things that
        decide what a measurement *means*. Cost, latency and the run's own identity are excluded: they
        describe the environment, not the measurement. Skill versions are excluded by default because a
        run that measures routing does not depend on them; `skills=True` includes them for the runs that
        do.
        """
        mine = {pin.dataset_id: pin for pin in self.pins}
        theirs = {pin.dataset_id: pin for pin in other.pins}
        for dataset_id in sorted(set(mine) | set(theirs)):
            left, right = mine.get(dataset_id), theirs.get(dataset_id)
            if left is None:
                return False, f"{dataset_id}: pinned only by the other run"
            if right is None:
                return False, f"{dataset_id}: pinned only by this run"
            if (left.version, left.digest) != (right.version, right.digest):
                return False, (
                    f"{dataset_id}: version {left.version}/{right.version}, "
                    f"digest {left.digest[:12]}…/{right.digest[:12]}…"
                )
        fields = ["model_route", "provider", "embedding_route", "index_snapshot", "harness_version"]
        for field in fields:
            left_value, right_value = getattr(self.versions, field), getattr(other.versions, field)
            if left_value != right_value:
                return False, f"{field}: {left_value!r}/{right_value!r}"
        if skills and self.versions.skill_versions != other.versions.skill_versions:
            return False, "skill_versions differ"
        return True, ""

    def as_mapping(self) -> dict[str, object]:
        """The record as it is written to evidence, digest included."""
        return {
            "run_id": self.run_id,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "pins": [pin.as_mapping() for pin in self.pins],
            "versions": self.versions.as_mapping(),
            "measurements": [item.as_mapping() for item in self.measurements],
            "cost": self.cost.as_mapping(),
            "digest": self.digest,
        }


def pins_for(datasets: Iterable[Dataset]) -> tuple[DatasetPin, ...]:
    """Pin a sequence of loaded datasets, in the order given."""
    return tuple(DatasetPin.of(dataset) for dataset in datasets)


def run_for(
    run_id: str,
    datasets: Sequence[Dataset],
    *,
    measurements: Sequence[Measurement],
    versions: ImplementationVersions | None = None,
    cost: CostLatency | None = None,
    started_at: str = "",
    finished_at: str = "",
) -> EvaluationRun:
    """Build a run record from the datasets it measured against."""
    return EvaluationRun(
        run_id=run_id,
        pins=pins_for(datasets),
        measurements=tuple(measurements),
        versions=versions if versions is not None else ImplementationVersions(),
        cost=cost if cost is not None else CostLatency(wall_time_ms=0),
        started_at=started_at,
        finished_at=finished_at,
    )


def _require_mapping(value: object, *, field: str, path: Path) -> Mapping[str, object]:
    if not isinstance(value, Mapping):
        raise RunError("VALIDATION_SCHEMA", RULE_RUN_SHAPE, f"{path}: {field} is not an object")
    return value


def _require_list(value: object, *, field: str, path: Path) -> Sequence[object]:
    if not isinstance(value, list):
        raise RunError("VALIDATION_SCHEMA", RULE_RUN_SHAPE, f"{path}: {field} is not a list")
    return value


def _int(value: object, *, field: str, path: Path) -> int:
    if isinstance(value, bool) or not isinstance(value, (int, str)):
        raise RunError("VALIDATION_SCHEMA", RULE_RUN_SHAPE, f"{path}: {field} is not an integer")
    try:
        return int(value)
    except ValueError as error:
        raise RunError("VALIDATION_SCHEMA", RULE_RUN_SHAPE, f"{path}: {field} is not an integer") from error


def load_run(path: Path) -> EvaluationRun:
    """Read a run record back; a record that does not hash to its recorded digest is refused.

    Every field is validated rather than coerced: a record read from disk is data, and a malformed one
    must fail closed with a typed error instead of producing a run that looks measured but is not.
    """
    try:
        decoded = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise RunError("NOT_FOUND", RULE_RUN_SHAPE, f"{path} is unreadable") from error
    except json.JSONDecodeError as error:
        raise RunError("VALIDATION_SCHEMA", RULE_RUN_SHAPE, f"{path} is not valid JSON") from error
    document = _require_mapping(decoded, field="run", path=path)

    pins: list[DatasetPin] = []
    for index, raw_pin in enumerate(_require_list(document.get("pins"), field="pins", path=path)):
        pin = _require_mapping(raw_pin, field=f"pins[{index}]", path=path)
        pins.append(
            DatasetPin(
                dataset_id=str(pin.get("dataset_id", "")),
                version=_int(pin.get("version"), field=f"pins[{index}].version", path=path),
                digest=str(pin.get("digest", "")),
                cases=_int(pin.get("cases", 0), field=f"pins[{index}].cases", path=path),
            )
        )

    measurements: list[Measurement] = []
    for index, raw_item in enumerate(
        _require_list(document.get("measurements"), field="measurements", path=path)
    ):
        item = _require_mapping(raw_item, field=f"measurements[{index}]", path=path)
        value = item.get("value", 0)
        if isinstance(value, bool) or not isinstance(value, (int, float, str)):
            raise RunError(
                "VALIDATION_SCHEMA", RULE_RUN_SHAPE, f"{path}: measurements[{index}].value is not a number"
            )
        measurements.append(
            Measurement(
                metric=str(item.get("metric", "")),
                value=float(value),
                unit=str(item.get("unit", "")),
                cases=_int(item.get("cases", 0), field=f"measurements[{index}].cases", path=path),
                notes=str(item.get("notes", "")),
            )
        )

    raw_versions = _require_mapping(document.get("versions", {}), field="versions", path=path)
    skill_versions: list[tuple[str, str]] = []
    for index, raw_pair in enumerate(
        _require_list(raw_versions.get("skill_versions", []), field="skill_versions", path=path)
    ):
        pair = _require_mapping(raw_pair, field=f"skill_versions[{index}]", path=path)
        skill_versions.append((str(pair.get("skill_id", "")), str(pair.get("version", ""))))
    versions = ImplementationVersions(
        model_route=str(raw_versions.get("model_route", "")),
        provider=str(raw_versions.get("provider", "")),
        embedding_route=str(raw_versions.get("embedding_route", "")),
        index_snapshot=str(raw_versions.get("index_snapshot", "")),
        skill_versions=tuple(skill_versions),
        harness_version=str(raw_versions.get("harness_version", "v1")),
    )

    raw_cost = _require_mapping(document.get("cost", {}), field="cost", path=path)
    cost = CostLatency(
        wall_time_ms=_int(raw_cost.get("wall_time_ms", 0), field="cost.wall_time_ms", path=path),
        model_calls=_int(raw_cost.get("model_calls", 0), field="cost.model_calls", path=path),
        input_tokens=_int(raw_cost.get("input_tokens", 0), field="cost.input_tokens", path=path),
        output_tokens=_int(raw_cost.get("output_tokens", 0), field="cost.output_tokens", path=path),
        cost_minor_units=_int(raw_cost.get("cost_minor_units", 0), field="cost.cost_minor_units", path=path),
        slowest_index_query_ms=_int(
            raw_cost.get("slowest_index_query_ms", 0), field="cost.slowest_index_query_ms", path=path
        ),
    )
    run = EvaluationRun(
        run_id=str(document.get("run_id", "")),
        pins=tuple(pins),
        measurements=tuple(measurements),
        versions=versions,
        cost=cost,
        started_at=str(document.get("started_at", "")),
        finished_at=str(document.get("finished_at", "")),
    )
    recorded = document.get("digest")
    if recorded is not None and str(recorded) != run.digest:
        raise RunError(
            "VALIDATION_SCHEMA",
            RULE_RUN_SHAPE,
            f"{path} records digest {recorded} but its content hashes to {run.digest}",
        )
    return run
