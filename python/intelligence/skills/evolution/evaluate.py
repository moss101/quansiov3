"""Deterministic evaluation of a candidate skill patch against its suite (CAP-008, INT-010 gate).

A candidate is evaluated the way the Skill Registry will use it: by resolution. Each case names a
task and the authority a caller holds, and whether the skill must resolve or be excluded there. A
candidate passes only if it behaves as its suite says, and — the protected rows of the gate — if it
loses no case the current version passes and needs nothing the current version did not. No model
call is made: resolution is a pure function of the manifest, the task and the snapshot.

The suite is the skill's own when its pack ships one (`packs/skills/<name>/evals/cases.yaml`,
named by the manifest's `eval_suite_id`); otherwise it is synthesized from the current version's
manifest — its examples resolve under exactly its needs, and each missing tool or capability
excludes it — so a registry skill without a pack is still held to its own declared behaviour.
"""

from __future__ import annotations

import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

import yaml

from intelligence.evaluation.datasets import Dataset, dataset_from_mapping
from intelligence.evaluation.gate import GateVerdict, ThresholdSet, evaluate_gate
from intelligence.evaluation.runs import CaseOutcome, EvaluationRun, Measurement, run_for
from intelligence.skills.evolution.patterns import EvolutionError
from intelligence.skills.models import CapabilitySnapshot, SkillVersionRef
from intelligence.skills.resolver import resolve

RULE_SUITE = "evolution.suite"
METRIC_PASS_RATE = "skill.case_pass_rate"
METRIC_REGRESSIONS = "skill.regressions"
METRIC_WIDENED = "skill.widened_needs"
THRESHOLDS_RELATIVE_PATH = Path("config") / "skill-evolution.yaml"
PACKS_RELATIVE_PATH = Path("packs") / "skills"


@dataclass(frozen=True, slots=True)
class SkillCase:
    """One resolution case: the task, the caller's authority, and the expected outcome."""

    case_id: str
    task: str
    tools: frozenset[str]
    capabilities: frozenset[str]
    expect_resolve: bool

    def as_payload(self) -> dict[str, object]:
        return {
            "task": self.task,
            "snapshot": {"tools": sorted(self.tools), "capabilities": sorted(self.capabilities)},
            "expect": "resolve" if self.expect_resolve else "exclude",
        }


@dataclass(frozen=True, slots=True)
class CandidateEvaluation:
    """The pinned run, the gate's verdict on it, and what it found."""

    run: EvaluationRun
    verdict: GateVerdict
    regressions: int
    widened_needs: tuple[str, ...]

    @property
    def passed(self) -> bool:
        return self.verdict.passed and not self.verdict.promotion_blocked


def evolution_config_path() -> Path:
    """`config/skill-evolution.yaml`, found by walking up from this module to the repository root."""
    for parent in Path(__file__).resolve().parents:
        candidate = parent / THRESHOLDS_RELATIVE_PATH
        if candidate.is_file():
            return candidate
    raise EvolutionError("INTERNAL", RULE_SUITE, f"{THRESHOLDS_RELATIVE_PATH} not found")


def default_thresholds() -> ThresholdSet:
    """The gate configuration in `config/skill-evolution.yaml`."""
    return ThresholdSet.load(evolution_config_path())


def pack_suite(eval_suite_id: str | None) -> tuple[SkillCase, ...]:
    """The cases of the built-in pack whose suite id the manifest names, when there is one."""
    if not eval_suite_id:
        return ()
    for parent in Path(__file__).resolve().parents:
        packs = parent / PACKS_RELATIVE_PATH
        if not packs.is_dir():
            continue
        for path in sorted(packs.glob("*/evals/cases.yaml")):
            document = yaml.safe_load(path.read_text(encoding="utf-8"))
            if isinstance(document, Mapping) and document.get("suite_id") == eval_suite_id:
                return tuple(_case(entry, path) for entry in document.get("cases") or [])
        break
    return ()


def synthesized_suite(current: SkillVersionRef) -> tuple[SkillCase, ...]:
    """Cases from the current manifest: its examples resolve under its needs; any missing need excludes."""
    manifest = current.manifest
    tools = frozenset(manifest.tool_needs)
    capabilities = frozenset(manifest.capability_needs)
    tasks = manifest.examples or (current.skill_name.replace("-", " "),)
    cases = [
        SkillCase(f"example_{index}_resolves", task, tools, capabilities, True)
        for index, task in enumerate(tasks, start=1)
    ]
    for tool in sorted(tools):
        cases.append(
            SkillCase(f"without_{_slug(tool)}_excluded", tasks[0], tools - {tool}, capabilities, False)
        )
    for capability in sorted(capabilities):
        cases.append(
            SkillCase(
                f"without_{_slug(capability)}_excluded", tasks[0], tools, capabilities - {capability}, False
            )
        )
    return tuple(cases)


def evaluate_candidate(
    current: SkillVersionRef,
    candidate: SkillVersionRef,
    *,
    run_id: str,
    thresholds: ThresholdSet,
    suite: Sequence[SkillCase] | None = None,
) -> CandidateEvaluation:
    """Evaluate `candidate` against the suite, relative to `current`, and gate it."""
    cases = (
        tuple(suite) if suite else pack_suite(current.manifest.eval_suite_id) or synthesized_suite(current)
    )
    if not cases:
        raise EvolutionError(
            "VALIDATION_SCHEMA", RULE_SUITE, f"skill {current.skill_id} has no evaluation cases"
        )
    dataset = _dataset(current, cases)
    current_passes = [_passes(current, case) for case in cases]
    candidate_passes = [_passes(candidate, case) for case in cases]
    regressions = sum(1 for was, now in zip(current_passes, candidate_passes, strict=True) if was and not now)
    widened = tuple(
        sorted(
            (set(candidate.manifest.tool_needs) - set(current.manifest.tool_needs))
            | (set(candidate.manifest.capability_needs) - set(current.manifest.capability_needs))
        )
    )
    passed = sum(candidate_passes)
    run = run_for(
        run_id,
        [dataset],
        measurements=[
            Measurement(METRIC_PASS_RATE, passed / len(cases), "ratio", cases=len(cases)),
            Measurement(METRIC_REGRESSIONS, float(regressions), "count", cases=len(cases)),
            Measurement(METRIC_WIDENED, float(len(widened)), "count", notes=", ".join(widened)),
        ],
        outcomes=[
            CaseOutcome(dataset_id=dataset.dataset_id, case_id=case.case_id, protected=True, passed=ok)
            for case, ok in zip(cases, candidate_passes, strict=True)
        ],
    )
    return CandidateEvaluation(
        run=run, verdict=evaluate_gate(run, thresholds), regressions=regressions, widened_needs=widened
    )


def _passes(version: SkillVersionRef, case: SkillCase) -> bool:
    resolution = resolve(
        task_text=case.task,
        snapshot=CapabilitySnapshot(tool_names=case.tools, capability_needs=case.capabilities),
        candidates=[_as_active(version)],
        max_skills=1,
    )
    return bool(resolution.resolved) == case.expect_resolve


def _as_active(version: SkillVersionRef) -> SkillVersionRef:
    """Evaluate the behaviour a version would have once active, whatever its ladder position."""
    return SkillVersionRef.build(
        skill_id=version.skill_id,
        version_id=version.version_id,
        skill_name=version.skill_name,
        semver=version.semver,
        status="active",
        manifest={
            "instructions": version.manifest.instructions,
            "examples": list(version.manifest.examples),
            "tool_needs": list(version.manifest.tool_needs),
            "capability_needs": list(version.manifest.capability_needs),
        },
    )


def _dataset(current: SkillVersionRef, cases: Sequence[SkillCase]) -> Dataset:
    return dataset_from_mapping(
        {
            "dataset_id": f"skill_{_slug(current.skill_name)}"[:64],
            "version": 1,
            "kind": "skill_behaviour",
            "cases": [
                {"case_id": case.case_id, "payload": case.as_payload(), "protected": True} for case in cases
            ],
            "notes": f"evaluation suite of {current.skill_id} at {current.version_id}",
        },
        source=f"skill {current.skill_id}",
    )


def _case(entry: object, path: Path) -> SkillCase:
    if not isinstance(entry, Mapping):
        raise EvolutionError("VALIDATION_SCHEMA", RULE_SUITE, f"{path}: a case is not an object")
    snapshot = entry.get("snapshot") or {}
    if not isinstance(snapshot, Mapping):
        raise EvolutionError("VALIDATION_SCHEMA", RULE_SUITE, f"{path}: a case snapshot is not an object")
    return SkillCase(
        case_id=str(entry.get("case_id", "")),
        task=str(entry.get("task", "")),
        tools=frozenset(str(tool) for tool in snapshot.get("tools") or []),
        capabilities=frozenset(str(need) for need in snapshot.get("capabilities") or []),
        expect_resolve=entry.get("expect") == "resolve",
    )


def _slug(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", value.lower()).strip("_") or "x"
