"""Versioned evaluation datasets: the pinned inputs a run is reproducible from (INT-010, DOSSIER §21).

An evaluation that cannot be reproduced measures nothing. This module is that guarantee: a dataset is
a *versioned, content-addressed* artifact — its identity, its version, its kind and every case in
order are hashed into one digest, and loading refuses a dataset whose content does not match the
digest the file records. So "reproducible from pinned inputs" is enforced at load time rather than
promised, and a run whose inputs moved is visibly a different run.

Five kinds, one per question the harness has to answer before a model, context or skill change is
promoted (the families INT-010 names):

* `route_quality` — does the same request resolve to the same route?
* `retrieval` — does retrieval find the pinned sources it should, and never a tenant's or a deleted
  source's?
* `grounding` — are the claims in an answer supported by the evidence cited?
* `tool_proposal` — are proposed tool calls valid against the tool declarations?
* `skill_behaviour` — does a skill resolve and behave as its manifest says?

A case's `payload` is deliberately opaque here: interpreting it is the metric's job, and keeping it
opaque means this module cannot invent case semantics the dataset did not state. A case marked
`protected` is a safety or recovery case, and the gate treats its regression as blocking regardless of
aggregate quality (DOSSIER §21.3's last row).
"""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

#: Refusal rules, named so a caller can tell which one fired.
RULE_DATASET_ID = "dataset.id"
RULE_DATASET_VERSION = "dataset.version"
RULE_DATASET_KIND = "dataset.kind"
RULE_DATASET_CASES = "dataset.cases"
RULE_DATASET_SHAPE = "dataset.shape"
RULE_DATASET_DIGEST = "dataset.digest"
RULE_DATASET_IO = "dataset.io"

#: The dataset families the task names; a dataset of any other kind is refused.
KINDS: tuple[str, ...] = (
    "route_quality",
    "retrieval",
    "grounding",
    "tool_proposal",
    "skill_behaviour",
)

DATASET_ID_PATTERN = re.compile(r"^[a-z][a-z0-9_]{2,63}$")

#: Every key a dataset file may carry, and every key a case may carry.
DATASET_KEYS = frozenset({"dataset_id", "version", "kind", "cases", "digest", "notes"})
CASE_KEYS = frozenset({"case_id", "payload", "protected", "tags"})


class DatasetError(ValueError):
    """A refused dataset, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class DatasetCase:
    """One pinned case: its identity, its inputs, and whether it is a protected safety case."""

    case_id: str
    payload: Mapping[str, object]
    protected: bool = False
    tags: tuple[str, ...] = ()

    def __post_init__(self) -> None:
        if not self.case_id.strip():
            raise DatasetError("VALIDATION_SCHEMA", RULE_DATASET_CASES, "a case needs an identity")
        if not isinstance(self.payload, Mapping):
            raise DatasetError(
                "VALIDATION_SCHEMA",
                RULE_DATASET_CASES,
                f"case {self.case_id!r} payload is {type(self.payload).__name__}, expected an object",
            )
        if not isinstance(self.protected, bool):
            raise DatasetError(
                "VALIDATION_SCHEMA", RULE_DATASET_CASES, f"case {self.case_id!r} protected is not a boolean"
            )
        if any(not isinstance(tag, str) or not tag.strip() for tag in self.tags):
            raise DatasetError(
                "VALIDATION_SCHEMA", RULE_DATASET_CASES, f"case {self.case_id!r} carries an empty tag"
            )

    def as_mapping(self) -> dict[str, object]:
        return {
            "case_id": self.case_id,
            "payload": dict(self.payload),
            "protected": self.protected,
            "tags": list(self.tags),
        }


@dataclass(frozen=True, slots=True)
class Dataset:
    """A versioned set of pinned cases, content-addressed by its own digest."""

    dataset_id: str
    version: int
    kind: str
    cases: tuple[DatasetCase, ...]
    notes: str = ""
    source: str = ""

    def __post_init__(self) -> None:
        if not DATASET_ID_PATTERN.match(self.dataset_id):
            raise DatasetError(
                "VALIDATION_SCHEMA",
                RULE_DATASET_ID,
                f"dataset id {self.dataset_id!r} must be a lower-case identifier",
            )
        if not isinstance(self.version, int) or isinstance(self.version, bool) or self.version < 1:
            raise DatasetError(
                "VALIDATION_SCHEMA", RULE_DATASET_VERSION, f"version {self.version!r} must be an integer >= 1"
            )
        if self.kind not in KINDS:
            raise DatasetError(
                "VALIDATION_SCHEMA",
                RULE_DATASET_KIND,
                f"{self.kind!r} is not one of {list(KINDS)}",
            )
        if not self.cases:
            raise DatasetError(
                "VALIDATION_SCHEMA", RULE_DATASET_CASES, f"dataset {self.dataset_id!r} has no cases"
            )
        identities = [case.case_id for case in self.cases]
        if len(set(identities)) != len(identities):
            raise DatasetError(
                "VALIDATION_SCHEMA",
                RULE_DATASET_CASES,
                f"dataset {self.dataset_id!r} repeats a case identity",
            )

    # -- identity ------------------------------------------------------------------

    @property
    def digest(self) -> str:
        """The content address: a pure function of the identity, version, kind and every case."""
        return hashlib.sha256(self.canonical_bytes()).hexdigest()

    def canonical_bytes(self) -> bytes:
        """The canonical rendering the digest is taken over (sorted keys, no whitespace)."""
        return json.dumps(
            {
                "dataset_id": self.dataset_id,
                "version": self.version,
                "kind": self.kind,
                "cases": [case.as_mapping() for case in self.cases],
            },
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")

    # -- reads ---------------------------------------------------------------------

    @property
    def protected_cases(self) -> tuple[DatasetCase, ...]:
        """The safety/recovery cases: a regression in one of these blocks promotion."""
        return tuple(case for case in self.cases if case.protected)

    def case(self, case_id: str) -> DatasetCase:
        for item in self.cases:
            if item.case_id == case_id:
                return item
        raise DatasetError("NOT_FOUND", RULE_DATASET_CASES, f"dataset has no case {case_id!r}")

    def tagged(self, tag: str) -> tuple[DatasetCase, ...]:
        return tuple(case for case in self.cases if tag in case.tags)

    def as_mapping(self) -> dict[str, object]:
        """The file form, including the digest that pins it."""
        return {
            "dataset_id": self.dataset_id,
            "version": self.version,
            "kind": self.kind,
            "notes": self.notes,
            "cases": [case.as_mapping() for case in self.cases],
            "digest": self.digest,
        }


def dataset_from_mapping(data: object, *, source: str = "", require_digest: bool = False) -> Dataset:
    """Build a dataset from decoded data, refusing an unknown shape or a drifted digest."""
    if not isinstance(data, Mapping):
        raise DatasetError(
            "VALIDATION_SCHEMA", RULE_DATASET_SHAPE, f"{source or 'dataset'} is not a JSON object"
        )
    unknown = sorted(set(data) - DATASET_KEYS)
    if unknown:
        raise DatasetError(
            "VALIDATION_SCHEMA", RULE_DATASET_SHAPE, f"{source or 'dataset'} carries unknown key(s) {unknown}"
        )
    raw_cases = data.get("cases")
    if not isinstance(raw_cases, list):
        raise DatasetError("VALIDATION_SCHEMA", RULE_DATASET_CASES, "cases must be a list")
    cases: list[DatasetCase] = []
    for entry in raw_cases:
        if not isinstance(entry, Mapping):
            raise DatasetError("VALIDATION_SCHEMA", RULE_DATASET_CASES, "a case is not an object")
        case_unknown = sorted(set(entry) - CASE_KEYS)
        if case_unknown:
            raise DatasetError(
                "VALIDATION_SCHEMA",
                RULE_DATASET_CASES,
                f"a case carries unknown key(s) {case_unknown}",
            )
        tags = entry.get("tags", [])
        if not isinstance(tags, list) or any(not isinstance(tag, str) for tag in tags):
            raise DatasetError("VALIDATION_SCHEMA", RULE_DATASET_CASES, "case tags must be strings")
        cases.append(
            DatasetCase(
                case_id=str(entry.get("case_id", "")),
                payload=entry.get("payload", {}),
                protected=bool(entry.get("protected", False)),
                tags=tuple(tags),
            )
        )
    raw_version = data.get("version")
    dataset = Dataset(
        dataset_id=str(data.get("dataset_id", "")),
        version=raw_version if isinstance(raw_version, int) and not isinstance(raw_version, bool) else 0,
        kind=str(data.get("kind", "")),
        cases=tuple(cases),
        notes=str(data.get("notes", "")),
        source=source,
    )
    recorded = data.get("digest")
    if recorded is not None and str(recorded) != dataset.digest:
        raise DatasetError(
            "VALIDATION_SCHEMA",
            RULE_DATASET_DIGEST,
            f"{source or dataset.dataset_id} records digest {recorded} but its content hashes to "
            f"{dataset.digest}; the pinned inputs have moved",
        )
    if recorded is None and require_digest:
        raise DatasetError(
            "VALIDATION_SCHEMA",
            RULE_DATASET_DIGEST,
            f"{source or dataset.dataset_id} carries no digest, so its inputs are not pinned",
        )
    return dataset


def load_dataset(path: Path, *, require_digest: bool = True) -> Dataset:
    """Load one dataset file, refusing drift from its recorded digest."""
    try:
        decoded = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise DatasetError("NOT_FOUND", RULE_DATASET_IO, f"{path} is unreadable") from error
    except json.JSONDecodeError as error:
        raise DatasetError("VALIDATION_SCHEMA", RULE_DATASET_IO, f"{path} is not valid JSON") from error
    return dataset_from_mapping(decoded, source=str(path), require_digest=require_digest)


def load_datasets(directory: Path, *, require_digest: bool = True) -> tuple[Dataset, ...]:
    """Load every pinned dataset in a directory, in file order, refusing a duplicate identity."""
    if not directory.is_dir():
        raise DatasetError("NOT_FOUND", RULE_DATASET_IO, f"{directory} is not a directory")
    datasets: list[Dataset] = []
    seen: set[tuple[str, int]] = set()
    for path in sorted(directory.glob("*.json")):
        dataset = load_dataset(path, require_digest=require_digest)
        key = (dataset.dataset_id, dataset.version)
        if key in seen:
            raise DatasetError(
                "CONFLICT_STATE",
                RULE_DATASET_SHAPE,
                f"{path.name} repeats dataset {dataset.dataset_id!r} version {dataset.version}",
            )
        seen.add(key)
        datasets.append(dataset)
    return tuple(datasets)


def datasets_of_kind(datasets: Sequence[Dataset], kind: str) -> tuple[Dataset, ...]:
    """The datasets of one family, in the order given."""
    return tuple(dataset for dataset in datasets if dataset.kind == kind)
