"""CAP-007: built-in skill and capability packs have schema, evals, and no policy bypass.

The suite loads the shipped packs under `packs/skills/` and `packs/capabilities/`.
A pack that widened tools, installed a daemon, or skipped its eval would fail these
checks. Tool availability is the INT-009 resolver: missing snapshot tools exclude.
"""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path

import pytest
import yaml

from intelligence.skills import CapabilitySnapshot, SkillManifest, SkillVersionRef, resolve

ROOT = Path(__file__).resolve().parents[3]
SKILLS = ROOT / "packs" / "skills"
CAPABILITIES = ROOT / "packs" / "capabilities"

REQUIRED_DOMAINS: tuple[str, ...] = (
    "research",
    "coding",
    "artifacts",
    "data-analysis",
    "cloud-devops",
    "business-ops",
)

SKILL_META_KEYS = frozenset(
    {
        "schema_version",
        "id",
        "name",
        "owner",
        "semver",
        "status",
        "provenance",
        # OPS-007 quarantine lifecycle, nested so it cannot collide with the scalar
        # `provenance` identity pointer above (scripts/ci/supply_chain/skills.py
        # GOVERNANCE_RECORD_KEY).
        "quarantine_record",
    }
)
PACK_KEYS = frozenset(
    {
        "schema_version",
        "id",
        "name",
        "owner",
        "semver",
        "status",
        "provenance",
        "contents",
        "evidence_requirements",
        "installs_daemon",
        "bypasses_runtime_policy",
        # OPS-007 quarantine lifecycle (pack.yaml has no separate meta.yaml sidecar,
        # so these sections are inline; `provenance.source`/`provenance.digest` fold
        # into the existing provenance dict above instead of a new top-level key).
        "quarantine",
        "review",
        "normalization",
        "evaluation",
        "approved",
    }
)
CONTENTS_KEYS = frozenset(
    {
        "knowledge_ids",
        "skill_version_ids",
        "tool_names",
        "connector_requirements",
        "policy_requirements",
        "rbac_requirements",
        "approval_requirements",
        "workflow_templates",
        "examples",
        "eval_suite_id",
        "compatibility",
    }
)
STATIC_TOOLS = frozenset(
    {
        "fs.read",
        "fs.write",
        "fs.patch",
        "terminal.exec",
        "process.spawn",
        "browser.navigate",
        "browser.click",
        "browser.type",
        "browser.extract",
        "browser.screenshot",
        "web.search",
        "web.fetch",
        "computer.read",
        "computer.click",
        "computer.type",
        "computer.clipboard",
        "computer.system_key",
        "artifact.create",
        "artifact.update",
        "work.propose_plan",
        "work.delegate",
        "user.ask",
        "memory.propose",
        "knowledge.cite",
    }
)
TOOL_PREFIXES = ("connector.", "scm.git.", "scm.pr.")
FORBIDDEN_SNIPPETS = (
    "systemctl enable",
    "systemctl start",
    "launchctl load",
    "daemon-reload",
    "installs_daemon: true",
    "bypasses_runtime_policy: true",
    "begin private key",
    "api_key:",
)


def _yaml(path: Path) -> dict[str, object]:
    raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    assert isinstance(raw, dict), path
    return raw


def _skill_dirs() -> list[Path]:
    return sorted(path for path in SKILLS.iterdir() if path.is_dir() and (path / "skill.yaml").is_file())


def _pack_dirs() -> list[Path]:
    return sorted(path for path in CAPABILITIES.iterdir() if path.is_dir() and (path / "pack.yaml").is_file())


def _known_tool(name: str) -> bool:
    return name in STATIC_TOOLS or name.startswith(TOOL_PREFIXES)


def assert_pack_safe(data: Mapping[str, object], *, source: str) -> None:
    """Refuse a pack that installs a daemon or bypasses runtime/tool policy."""
    if data.get("installs_daemon") is not False:
        raise AssertionError(f"{source}: packs must set installs_daemon: false")
    if data.get("bypasses_runtime_policy") is not False:
        raise AssertionError(f"{source}: packs must set bypasses_runtime_policy: false")
    text = yaml.safe_dump(dict(data)).lower()
    for snippet in FORBIDDEN_SNIPPETS:
        if snippet in text:
            raise AssertionError(f"{source}: forbidden policy/daemon snippet {snippet!r}")


def _version_from_skill(skill_dir: Path) -> SkillVersionRef:
    meta = _yaml(skill_dir / "meta.yaml")
    manifest = SkillManifest.parse(_yaml(skill_dir / "skill.yaml"))
    return SkillVersionRef.build(
        skill_id=str(meta["id"]),
        version_id=f"sklv_{skill_dir.name.replace('-', '_')}_1",
        skill_name=str(meta["name"]),
        semver=str(meta["semver"]),
        status=str(meta["status"]),
        manifest={
            "instructions": manifest.instructions,
            "examples": list(manifest.examples),
            "tool_needs": list(manifest.tool_needs),
            "capability_needs": list(manifest.capability_needs),
            "eval_suite_id": manifest.eval_suite_id,
            "compatibility": list(manifest.compatibility),
            "recovery_guidance": manifest.recovery_guidance,
        },
        provenance=str(meta["provenance"]),
    )


def test_pack_schema_and_eval_checks() -> None:
    """Named test: pack schema/eval checks. Owner, version, provenance, passing eval."""
    skill_dirs = _skill_dirs()
    names = {path.name for path in skill_dirs}
    missing = [name for name in REQUIRED_DOMAINS if name not in names]
    assert missing == [], f"missing day-one skill packs: {missing}"
    pack_names = {path.name for path in _pack_dirs()}
    missing_packs = [name for name in REQUIRED_DOMAINS if name not in pack_names]
    assert missing_packs == [], f"missing day-one capability packs: {missing_packs}"

    for skill_dir in skill_dirs:
        meta = _yaml(skill_dir / "meta.yaml")
        extra = set(meta) - SKILL_META_KEYS
        assert extra == set(), f"{skill_dir.name} meta unknown keys {extra}"
        for key in ("owner", "semver", "provenance", "id", "name", "status"):
            assert str(meta[key]).strip(), f"{skill_dir.name} missing {key}"
        assert str(meta["id"]).startswith("skl_")
        assert meta["status"] == "active"
        assert str(meta["provenance"]).startswith("pack://")
        manifest = SkillManifest.parse(_yaml(skill_dir / "skill.yaml"))
        assert manifest.eval_suite_id
        assert manifest.recovery_guidance
        assert manifest.tool_needs
        cases_path = skill_dir / "evals" / "cases.yaml"
        assert cases_path.is_file(), f"{skill_dir.name} has no eval cases"
        suite = _yaml(cases_path)
        assert suite.get("suite_id") == manifest.eval_suite_id
        cases = suite.get("cases")
        assert isinstance(cases, list) and cases, f"{skill_dir.name} eval is empty"
        version = _version_from_skill(skill_dir)
        for case in cases:
            assert isinstance(case, dict)
            snap_raw = case["snapshot"]
            assert isinstance(snap_raw, dict)
            tools = snap_raw["tools"]
            caps = snap_raw["capabilities"]
            assert isinstance(tools, list) and isinstance(caps, list)
            resolution = resolve(
                task_text=str(case["task"]),
                snapshot=CapabilitySnapshot(
                    tool_names=frozenset(str(item) for item in tools),
                    capability_needs=frozenset(str(item) for item in caps),
                ),
                candidates=[version],
            )
            if case["expect"] == "resolve":
                assert resolution.resolved, f"{skill_dir.name}/{case['case_id']} should resolve"
            else:
                assert resolution.resolved == (), f"{skill_dir.name}/{case['case_id']} should exclude"

    for pack_dir in _pack_dirs():
        pack = _yaml(pack_dir / "pack.yaml")
        extra = set(pack) - PACK_KEYS
        assert extra == set(), f"{pack_dir.name} pack unknown keys {extra}"
        assert pack["owner"] and pack["semver"]
        provenance = pack["provenance"]
        assert isinstance(provenance, dict) and provenance.get("kind") == "builtin"
        assert str(provenance.get("ref", "")).startswith("pack://")
        contents = pack["contents"]
        assert isinstance(contents, dict)
        extra_c = set(contents) - CONTENTS_KEYS
        assert extra_c == set(), f"{pack_dir.name} contents unknown keys {extra_c}"
        assert contents.get("eval_suite_id")
        assert pack["status"] == "published"
        assert_pack_safe(pack, source=str(pack_dir / "pack.yaml"))


def test_capability_scan_refuses_unknown_tools_and_daemons() -> None:
    """Named test: capability scan. Unknown tools, daemon install, and policy bypass fail."""
    for skill_dir in _skill_dirs():
        manifest = SkillManifest.parse(_yaml(skill_dir / "skill.yaml"))
        for tool in manifest.tool_needs:
            assert _known_tool(tool), f"{skill_dir.name} declares unknown tool {tool}"
        text = (skill_dir / "skill.yaml").read_text(encoding="utf-8").lower()
        for snippet in FORBIDDEN_SNIPPETS:
            assert snippet not in text, f"{skill_dir.name} contains {snippet!r}"
    for pack_dir in _pack_dirs():
        pack = _yaml(pack_dir / "pack.yaml")
        contents = pack["contents"]
        assert isinstance(contents, dict)
        tools = contents["tool_names"]
        assert isinstance(tools, list)
        for tool in tools:
            assert _known_tool(str(tool)), f"{pack_dir.name} pack unknown tool {tool}"
        assert_pack_safe(pack, source=str(pack_dir / "pack.yaml"))
        # Pack tools must cover the matching skill's needs (cannot silently drop a need).
        skill_dir = SKILLS / pack_dir.name
        if skill_dir.is_dir():
            needed = set(SkillManifest.parse(_yaml(skill_dir / "skill.yaml")).tool_needs)
            assert needed <= {str(item) for item in tools}, f"{pack_dir.name} pack drops skill tools"

    with pytest.raises(AssertionError, match="installs_daemon"):
        assert_pack_safe(
            {
                "installs_daemon": True,
                "bypasses_runtime_policy": False,
            },
            source="synthetic-daemon",
        )
    with pytest.raises(AssertionError, match="bypasses_runtime_policy"):
        assert_pack_safe(
            {
                "installs_daemon": False,
                "bypasses_runtime_policy": True,
            },
            source="synthetic-bypass",
        )


def test_tool_availability_uses_the_resolver() -> None:
    """Named test: tool availability. A pack cannot grant a tool the caller lacks."""
    for skill_dir in _skill_dirs():
        version = _version_from_skill(skill_dir)
        permitted = resolve(
            task_text=" ".join(version.manifest.examples) or version.skill_name,
            snapshot=CapabilitySnapshot(
                tool_names=frozenset(version.manifest.tool_needs),
                capability_needs=frozenset(version.manifest.capability_needs),
            ),
            candidates=[version],
        )
        assert permitted.resolved_ids() == (version.version_id,), skill_dir.name
        denied = resolve(
            task_text=" ".join(version.manifest.examples) or version.skill_name,
            snapshot=CapabilitySnapshot(tool_names=frozenset(), capability_needs=frozenset()),
            candidates=[version],
        )
        assert denied.resolved == ()
        assert any(item.rule_id == "skill.tool_not_available" for item in denied.excluded)
