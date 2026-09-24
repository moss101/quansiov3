"""From verified experience to evaluated candidates (CAP-008): the plane's whole proposal.

The runtime hands over what it knows as canonical fact — verified run outcomes, each with its
evidence identity and digest and the skill version the run was given — and the registry's ACTIVE
versions. The plane:

1. groups the records into repeated patterns (`detect_patterns`; fewer than `min_repeats` is an
   anecdote);
2. turns each pattern into one CANDIDATE patch of the version the runs used: a success pattern
   adds the verified tasks as examples, a failure or recovery pattern adds what the evidence showed
   to the recovery guidance;
3. evaluates the candidate against its suite and gates it (`evaluate.evaluate_candidate`);
4. returns the candidate with provenance that names both the evidence and the evaluation.

Nothing here writes anything, and nothing here is ever ACTIVE: the runtime registers what it
receives as a CANDIDATE version, and promotion past EVALUATING needs the passing evaluation this
provenance records.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

import yaml

from intelligence.evaluation.gate import ThresholdSet
from intelligence.skills.evolution.candidates import CandidatePatch, bind_evaluation
from intelligence.skills.evolution.evaluate import (
    CandidateEvaluation,
    evaluate_candidate,
    evolution_config_path,
)
from intelligence.skills.evolution.patterns import (
    EvidenceKind,
    EvidenceRecord,
    EvolutionError,
    Pattern,
    detect_patterns,
)
from intelligence.skills.models import SkillManifest, SkillStatus, SkillVersionRef

RULE_UNKNOWN_VERSION = "evolution.unknown_version"
ORIGIN = "evolution"
MAX_GUIDANCE_DETAILS = 5


@dataclass(frozen=True, slots=True)
class EvolutionProposal:
    """An evaluated candidate and the provenance the registry stores with it."""

    patch: CandidatePatch
    evaluation: CandidateEvaluation
    records: tuple[EvidenceRecord, ...]

    @property
    def provenance(self) -> dict[str, object]:
        verdict = self.evaluation.verdict
        return {
            "origin": ORIGIN,
            "from_version_id": self.patch.from_version_id,
            "pattern_kind": self.patch.pattern_kind,
            "evidence": [
                {"evidence_id": record.evidence_id, "digest": record.digest, "run_id": record.run_id}
                for record in self.records
            ],
            "evaluation": {
                "eval_run_id": self.evaluation.run.run_id,
                "digest": self.evaluation.run.digest,
                "passed": self.evaluation.passed,
                "reason": verdict.reason,
                "regressions": self.evaluation.regressions,
                "widened_needs": list(self.evaluation.widened_needs),
            },
        }


def propose_candidates(
    records: Sequence[EvidenceRecord],
    active_versions: Sequence[SkillVersionRef],
    *,
    thresholds: ThresholdSet,
    min_repeats: int,
) -> tuple[EvolutionProposal, ...]:
    """Patterns → evaluated candidates, one per pattern that changes its skill."""
    versions: Mapping[str, SkillVersionRef] = {version.skill_id: version for version in active_versions}
    proposals: list[EvolutionProposal] = []
    for pattern in detect_patterns(records, min_repeats=min_repeats):
        current = versions.get(pattern.skill_id)
        if current is None:
            raise EvolutionError(
                "VALIDATION_SCHEMA",
                RULE_UNKNOWN_VERSION,
                f"records name skill {pattern.skill_id}, which has no active version to evolve",
            )
        matched = tuple(
            record
            for record in records
            if record.skill_id == pattern.skill_id and record.kind is pattern.kind
        )
        manifest = _patched(current.manifest, pattern, matched)
        if manifest is None:
            continue  # the pattern teaches this version nothing it does not already say
        candidate = SkillVersionRef(
            skill_id=current.skill_id,
            version_id=f"proposed:{current.version_id}:{pattern.kind.value}",
            skill_name=current.skill_name,
            semver=_bump_patch(current.semver),
            status=SkillStatus.CANDIDATE,
            manifest=manifest,
            provenance=f"evidence:{pattern.evidence_ids[0]}",
        )
        patch = CandidatePatch(
            skill_id=current.skill_id,
            from_version_id=current.version_id,
            proposed=candidate,
            evidence_ids=pattern.evidence_ids,
            evidence_digests=pattern.digests,
            pattern_kind=pattern.kind.value,
        )
        evaluation = evaluate_candidate(
            current,
            candidate,
            run_id=f"skill-evolution:{current.version_id}:{pattern.kind.value}:{candidate.semver}",
            thresholds=thresholds,
        )
        proposals.append(
            EvolutionProposal(
                patch=bind_evaluation(patch, evaluation.run), evaluation=evaluation, records=matched
            )
        )
    return tuple(proposals)


def _patched(
    manifest: SkillManifest, pattern: Pattern, records: Sequence[EvidenceRecord]
) -> SkillManifest | None:
    if pattern.kind is EvidenceKind.SUCCESS:
        learned = tuple(
            dict.fromkeys(
                record.task.strip()
                for record in records
                if record.task.strip() and record.task.strip() not in manifest.examples
            )
        )
        if not learned:
            return None
        return _with(manifest, examples=manifest.examples + learned)
    details = tuple(dict.fromkeys(record.detail.strip() for record in records if record.detail.strip()))
    if not details:
        return None
    label = "Known failure" if pattern.kind is EvidenceKind.FAILURE else "Recovered before"
    lesson = f"{label} ({pattern.count} verified runs): " + "; ".join(details[:MAX_GUIDANCE_DETAILS])
    if manifest.recovery_guidance and lesson in manifest.recovery_guidance:
        return None
    guidance = f"{manifest.recovery_guidance}\n{lesson}" if manifest.recovery_guidance else lesson
    return _with(manifest, recovery_guidance=guidance)


def _with(manifest: SkillManifest, **changes: object) -> SkillManifest:
    fields: dict[str, object] = {
        "instructions": manifest.instructions,
        "examples": list(manifest.examples),
        "tool_needs": list(manifest.tool_needs),
        "capability_needs": list(manifest.capability_needs),
        "eval_suite_id": manifest.eval_suite_id,
        "compatibility": list(manifest.compatibility),
        "recovery_guidance": manifest.recovery_guidance,
    }
    for key, value in changes.items():
        fields[key] = list(value) if isinstance(value, tuple) else value
    return SkillManifest.parse({key: value for key, value in fields.items() if value is not None})


def _bump_patch(semver: str) -> str:
    parts = semver.split(".")
    if len(parts) != 3 or not all(part.isdigit() for part in parts):
        raise EvolutionError(
            "VALIDATION_SCHEMA", RULE_UNKNOWN_VERSION, f"semver {semver!r} is not MAJOR.MINOR.PATCH"
        )
    return f"{parts[0]}.{parts[1]}.{int(parts[2]) + 1}"


# -- the wire (`ProposeSkillCandidates`) ---------------------------------------------------------


def records_from_json(raw: Sequence[str]) -> tuple[EvidenceRecord, ...]:
    """`{evidence_id, skill_id, run_id, kind, digest, detail?, task?}` records, refusing any other shape."""
    records = []
    for entry in raw:
        decoded = _object(entry, "an evidence record")
        records.append(
            EvidenceRecord(
                evidence_id=str(decoded.get("evidence_id", "")),
                skill_id=str(decoded.get("skill_id", "")),
                run_id=str(decoded.get("run_id", "")),
                kind=EvidenceKind.parse(decoded.get("kind")),
                digest=str(decoded.get("digest", "")),
                detail=str(decoded.get("detail", "")),
                task=str(decoded.get("task", "")),
            )
        )
    return tuple(records)


def versions_from_json(raw: Sequence[str]) -> tuple[SkillVersionRef, ...]:
    """`{skill_id, version_id, name, semver, status, manifest}` versions, as the registry lists them."""
    return tuple(
        SkillVersionRef.build(
            skill_id=str(decoded.get("skill_id", "")),
            version_id=str(decoded.get("version_id", "")),
            skill_name=str(decoded.get("name", "")),
            semver=str(decoded.get("semver", "")),
            status=decoded.get("status", ""),
            manifest=decoded.get("manifest"),
        )
        for decoded in (_object(entry, "a skill version") for entry in raw)
    )


def candidate_json(proposal: EvolutionProposal) -> str:
    """What the registry stores: the proposed version and its evidence+evaluation provenance."""
    manifest = proposal.patch.proposed.manifest
    return json.dumps(
        {
            "skill_id": proposal.patch.skill_id,
            "from_version_id": proposal.patch.from_version_id,
            "semver": proposal.patch.proposed.semver,
            "manifest": {
                "instructions": manifest.instructions,
                "examples": list(manifest.examples),
                "tool_needs": list(manifest.tool_needs),
                "capability_needs": list(manifest.capability_needs),
                **({"eval_suite_id": manifest.eval_suite_id} if manifest.eval_suite_id else {}),
                "compatibility": list(manifest.compatibility),
                **({"recovery_guidance": manifest.recovery_guidance} if manifest.recovery_guidance else {}),
            },
            "provenance": proposal.provenance,
        },
        sort_keys=True,
    )


def evolution_settings() -> tuple[ThresholdSet, int]:
    """The gate and the pattern threshold (`min_repeats`) from `config/skill-evolution.yaml`."""
    path = evolution_config_path()
    raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    min_repeats = raw.get("min_repeats") if isinstance(raw, dict) else None
    if not isinstance(min_repeats, int) or isinstance(min_repeats, bool) or min_repeats < 2:
        raise EvolutionError("INTERNAL", RULE_UNKNOWN_VERSION, f"{path}: min_repeats must be an integer >= 2")
    return ThresholdSet.from_mapping(raw, source=str(path)), min_repeats


def _object(entry: str, what: str) -> dict[str, Any]:
    try:
        decoded = json.loads(entry)
    except json.JSONDecodeError as error:
        raise EvolutionError(
            "VALIDATION_SCHEMA", RULE_UNKNOWN_VERSION, f"{what} is not valid JSON"
        ) from error
    if not isinstance(decoded, dict):
        raise EvolutionError("VALIDATION_SCHEMA", RULE_UNKNOWN_VERSION, f"{what} must be a JSON object")
    return decoded
