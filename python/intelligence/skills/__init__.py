"""Skill Registry and task-scoped resolver (INT-009, DOMAIN.md §11.5).

Canonical owner: `python/intelligence/skills`. Skills are versioned procedural assets: only
`ACTIVE` versions resolve, a version is excluded unless the caller's own snapshot already permits
everything it needs, and resolution is bounded and deterministic. The control-plane store that
holds the metadata, provenance and promotion states lives in `crates/server` (INT-009's Rust half).
"""

from __future__ import annotations

from intelligence.skills.models import (
    MANIFEST_KEYS,
    RESOLVABLE_STATE,
    SKILL_STATES,
    CapabilitySnapshot,
    SkillError,
    SkillManifest,
    SkillStatus,
    SkillVersionRef,
)
from intelligence.skills.resolver import (
    ExcludedSkill,
    Resolution,
    resolve,
)

__all__ = [
    "MANIFEST_KEYS",
    "RESOLVABLE_STATE",
    "SKILL_STATES",
    "CapabilitySnapshot",
    "ExcludedSkill",
    "Resolution",
    "SkillError",
    "SkillManifest",
    "SkillStatus",
    "SkillVersionRef",
    "resolve",
]
