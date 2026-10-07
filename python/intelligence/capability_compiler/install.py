"""Pack install: content-hash revision, verification and compilation (ADP-008, DOMAIN.md 11.6).

A pack is installed *by revision*: the sha256 of its canonical content (manifest and files). This module is
the plane's half of an install. It computes and checks the revision, verifies the pack against the CAP-004
rules (credential-shaped content is refused, the quarantine record must be cleared, no daemon, no policy
bypass) and compiles it against the Tool Registry's visibility (a pack may name only tools a model can see, so
it can never widen the model's reach) and the CAP-005 qualification gate. It returns a `PackCompilation`:
diagnostics and a resolution. It writes nothing: activation, retention, rollback and the install report
are the server's (`crates/server/src/control/packs`), and the server recomputes the revision itself.

Canonical content is JSON with sorted keys, no insignificant whitespace and non-ASCII kept as UTF-8, over
``{"files": {...}, "manifest": {...}}`` where the manifest is taken without its governance record (quarantine,
review, normalization, evaluation, approved) and without ``provenance.digest``: re-reviewing a pack does not
change what executes, so it does not change its revision. Numbers must be integers so the two implementations
cannot disagree about a float; a shared corpus (``tests/contract/fixtures/packs/revision_corpus.json``) pins
them.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any

from .compiler import CREDENTIAL_PATTERNS, RULE_CREDENTIAL
from .lifecycle import Qualification

#: The sections of a pack manifest that record its governance, not its content.
GOVERNANCE: tuple[str, ...] = ("quarantine", "review", "normalization", "evaluation", "approved")

PHASE_VERIFY = "verify"
PHASE_COMPILE = "compile"

#: Closed vocabularies (DOMAIN.md 11.6): an unknown key is a refusal, not an ignored extra.
MANIFEST_KEYS = frozenset(
    {
        "schema_version",
        "id",
        "name",
        "owner",
        "semver",
        "status",
        "provenance",
        "quarantine",
        "review",
        "normalization",
        "evaluation",
        "approved",
        "contents",
        "evidence_requirements",
        "installs_daemon",
        "bypasses_runtime_policy",
    }
)
REQUIRED_KEYS = frozenset(
    {"schema_version", "name", "semver", "provenance", "quarantine", "review", "evaluation", "contents"}
)
CONTENT_LIST_KEYS = (
    "knowledge_ids",
    "skill_version_ids",
    "tool_names",
    "connector_requirements",
    "policy_requirements",
    "rbac_requirements",
    "approval_requirements",
    "examples",
    "compatibility",
)
CONTENT_KEYS = frozenset({*CONTENT_LIST_KEYS, "workflow_templates", "eval_suite_id"})

_NAME = re.compile(r"^[a-z][a-z0-9_-]{0,63}$")
_SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
_REVISION = re.compile(r"^sha256:[0-9a-f]{64}$")
_TOOL = re.compile(r"^[a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+$")
_PATH = re.compile(r"^[A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)*$")


class PackContentError(ValueError):
    """The content cannot be put in canonical form (a non-integer number, a non-JSON value)."""


def canonical_json(value: object) -> str:
    """The canonical text of a JSON value: sorted keys, no whitespace, UTF-8 kept, integers only."""
    _reject_floats(value)
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _reject_floats(value: object, pointer: str = "") -> None:
    if isinstance(value, bool) or value is None or isinstance(value, (int, str)):
        return
    if isinstance(value, float):
        raise PackContentError(f"{pointer or '/'}: numbers in a pack must be integers")
    if isinstance(value, Mapping):
        for key, item in value.items():
            if not isinstance(key, str):
                raise PackContentError(f"{pointer or '/'}: object keys must be strings")
            _reject_floats(item, f"{pointer}/{key}")
        return
    if isinstance(value, Sequence) and not isinstance(value, (bytes, bytearray)):
        for index, item in enumerate(value):
            _reject_floats(item, f"{pointer}/{index}")
        return
    raise PackContentError(f"{pointer or '/'}: {type(value).__name__} is not a JSON value")


def content_manifest(manifest: Mapping[str, object]) -> dict[str, object]:
    """The manifest as content: without its governance record and without ``provenance.digest``."""
    content = {key: value for key, value in manifest.items() if key not in GOVERNANCE}
    provenance = content.get("provenance")
    if isinstance(provenance, Mapping):
        content["provenance"] = {key: value for key, value in provenance.items() if key != "digest"}
    return content


def manifest_digest(manifest: Mapping[str, object]) -> str:
    """What ``provenance.digest`` must name (CAP-007): the manifest's content without its record."""
    return "sha256:" + hashlib.sha256(canonical_json(content_manifest(manifest)).encode("utf-8")).hexdigest()


def revision_of(manifest: Mapping[str, object], files: Mapping[str, str]) -> str:
    """The pack's content-hash revision: sha256 of its canonical manifest content and files."""
    content = {"files": dict(files), "manifest": content_manifest(manifest)}
    return "sha256:" + hashlib.sha256(canonical_json(content).encode("utf-8")).hexdigest()


@dataclass(frozen=True, slots=True)
class Diagnostic:
    """One finding. ``message`` never quotes content that matched a credential rule."""

    phase: str
    rule: str
    message: str
    pointer: str = ""
    severity: str = "error"

    def as_mapping(self) -> dict[str, str]:
        out = {"phase": self.phase, "rule": self.rule, "severity": self.severity, "message": self.message}
        if self.pointer:
            out["pointer"] = self.pointer
        return out


@dataclass(frozen=True, slots=True)
class ToolVisibility:
    """What the pack compiler is told about one registry tool."""

    name: str
    model_visible: bool
    host_callable: bool = False

    @classmethod
    def parse(cls, raw: str) -> ToolVisibility:
        data = json.loads(raw)
        if not isinstance(data, dict) or not isinstance(data.get("name"), str):
            raise ValueError("a tool entry is {name, model_visible, host_callable}")
        visible, host = data.get("model_visible"), data.get("host_callable", False)
        if not isinstance(visible, bool) or not isinstance(host, bool):
            raise ValueError(f"tool {data['name']!r}: model_visible and host_callable are booleans")
        return cls(name=data["name"], model_visible=visible, host_callable=host)


@dataclass(frozen=True, slots=True)
class InstallSettings:
    """Bounds and claims the server hands the plane with a pack."""

    expected_revision: str | None = None
    max_files: int = 64
    max_file_bytes: int = 262_144
    max_content_bytes: int = 1_048_576

    @classmethod
    def parse(cls, raw: str | None) -> InstallSettings:
        if not raw:
            return cls()
        data = json.loads(raw)
        if not isinstance(data, dict):
            raise ValueError("settings must be an object")
        expected = data.get("expected_revision")
        if expected is not None and not (isinstance(expected, str) and _REVISION.match(expected)):
            raise ValueError("expected_revision must be sha256:<64 hex digits>")
        out = {}
        for key in ("max_files", "max_file_bytes", "max_content_bytes"):
            if key in data:
                value = data[key]
                if not isinstance(value, int) or isinstance(value, bool) or value < 1:
                    raise ValueError(f"{key} must be a positive integer")
                out[key] = value
        return cls(expected_revision=expected, **out)


@dataclass(slots=True)
class PackCompilation:
    """The plane's answer: a revision, what it found and what the pack resolves to."""

    revision: str
    manifest_digest: str
    diagnostics: list[Diagnostic] = field(default_factory=list)
    resolution: dict[str, object] = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        return not any(item.severity == "error" for item in self.diagnostics)

    @property
    def failed_phase(self) -> str | None:
        """The earliest phase with an error: verification comes before compilation."""
        for phase in (PHASE_VERIFY, PHASE_COMPILE):
            if any(item.phase == phase and item.severity == "error" for item in self.diagnostics):
                return phase
        return None

    def as_mapping(self) -> dict[str, object]:
        return {
            "schema_version": "v1",
            "ok": self.ok,
            "failed_phase": self.failed_phase,
            "revision": self.revision,
            "manifest_digest": self.manifest_digest,
            "diagnostics": [item.as_mapping() for item in self.diagnostics],
            "resolution": self.resolution,
        }


def _as_dict(value: object) -> dict[str, Any]:
    """`value` when it is an object, else an empty one: for a section verification has already shaped."""
    return value if isinstance(value, dict) else {}


def _strings(value: object) -> bool:
    return isinstance(value, list) and all(isinstance(item, str) for item in value)


def _check_shape(manifest: object, files: object, settings: InstallSettings) -> list[Diagnostic]:
    """``pack.schema``: the closed shape of a pack and its files."""
    out: list[Diagnostic] = []

    def refuse(message: str, pointer: str = "") -> None:
        out.append(Diagnostic(PHASE_VERIFY, "pack.schema", message, pointer))

    if not isinstance(manifest, dict):
        refuse("the manifest must be an object")
        return out
    for key in sorted(set(manifest) - MANIFEST_KEYS):
        refuse("not a field of a pack manifest", f"/{key}")
    for key in sorted(REQUIRED_KEYS - set(manifest)):
        refuse("required field is missing", f"/{key}")
    if manifest.get("schema_version") != "v1":
        refuse("schema_version must be v1", "/schema_version")
    if not (isinstance(manifest.get("name"), str) and _NAME.match(manifest["name"])):
        refuse("name must be lowercase letters, digits, - or _ and start with a letter", "/name")
    if not (isinstance(manifest.get("semver"), str) and _SEMVER.match(manifest["semver"])):
        refuse("semver must be major.minor.patch", "/semver")
    for flag in ("installs_daemon", "bypasses_runtime_policy"):
        if flag in manifest and not isinstance(manifest[flag], bool):
            refuse(f"{flag} must be a boolean", f"/{flag}")
    contents = manifest.get("contents")
    if isinstance(contents, dict):
        for key in sorted(set(contents) - CONTENT_KEYS):
            refuse("not a field of a pack's contents", f"/contents/{key}")
        for key in CONTENT_LIST_KEYS:
            if key in contents and not _strings(contents[key]):
                refuse(f"{key} must be a list of strings", f"/contents/{key}")
        if "workflow_templates" in contents and not isinstance(contents["workflow_templates"], list):
            refuse("workflow_templates must be a list", "/contents/workflow_templates")
        if "eval_suite_id" in contents and not isinstance(contents["eval_suite_id"], str):
            refuse("eval_suite_id must be a string", "/contents/eval_suite_id")
    elif "contents" in manifest:
        refuse("contents must be an object", "/contents")
    for section in ("provenance", "quarantine", "review", "evaluation"):
        if section in manifest and not isinstance(manifest[section], dict):
            refuse(f"{section} must be an object", f"/{section}")
    if not isinstance(files, dict):
        refuse("files must be an object of path to text", "/files")
        return out
    if len(files) > settings.max_files:
        refuse(f"a pack holds at most {settings.max_files} files", "/files")
    total = 0
    for path, text in sorted(files.items()):
        pointer = f"/files/{path}"
        if (
            not (isinstance(path, str) and _PATH.match(path))
            or ".." in path.split("/")
            or path.startswith(".")
        ):
            refuse("a file path is relative, without .. and without a leading dot", pointer)
            continue
        if not isinstance(text, str):
            refuse("a file's content must be text", pointer)
            continue
        size = len(text.encode("utf-8"))
        total += size
        if size > settings.max_file_bytes:
            refuse(f"a file is at most {settings.max_file_bytes} bytes", pointer)
    if total > settings.max_content_bytes:
        refuse(f"a pack is at most {settings.max_content_bytes} bytes", "/files")
    return out


def _check_record(manifest: Mapping[str, object]) -> list[Diagnostic]:
    """The quarantine rules (OPS-007, CAP-007): no install without a cleared, reviewed, evaluated record."""
    out: list[Diagnostic] = []

    def refuse(rule: str, message: str, pointer: str) -> None:
        out.append(Diagnostic(PHASE_VERIFY, rule, message, pointer))

    quarantine = manifest.get("quarantine")
    if isinstance(quarantine, Mapping) and quarantine.get("status") != "cleared":
        refuse("pack.quarantine", "the pack is still in quarantine", "/quarantine/status")
    review = manifest.get("review")
    if isinstance(review, Mapping) and review.get("outcome") != "approved":
        refuse("pack.quarantine", "the pack was not approved on review", "/review/outcome")
    evaluation = manifest.get("evaluation")
    if isinstance(evaluation, Mapping) and evaluation.get("result") != "pass":
        refuse("pack.quarantine", "the pack's evaluation did not pass", "/evaluation/result")
    if manifest.get("installs_daemon") is not False and "installs_daemon" in manifest:
        refuse("pack.daemon", "a pack never installs a daemon", "/installs_daemon")
    if manifest.get("bypasses_runtime_policy") is not False and "bypasses_runtime_policy" in manifest:
        refuse("pack.bypass", "a pack never bypasses runtime or tool policy", "/bypasses_runtime_policy")
    return out


def _credential_findings(manifest: Mapping[str, object], files: Mapping[str, str]) -> list[Diagnostic]:
    """CAP-004: credential-shaped content is refused. A finding names where, never what."""
    out: list[Diagnostic] = []
    texts = [("/manifest", canonical_json(content_manifest(manifest)))]
    texts.extend((f"/files/{path}", text) for path, text in sorted(files.items()))
    for pointer, text in texts:
        count = sum(1 for pattern in CREDENTIAL_PATTERNS if pattern.search(text))
        if count:
            out.append(
                Diagnostic(
                    PHASE_VERIFY,
                    RULE_CREDENTIAL.replace("compiler.", "pack."),
                    f"credential-shaped content ({count} rule{'s' if count != 1 else ''}); raw credentials "
                    "are excluded from packs",
                    pointer,
                )
            )
    return out


def verify_pack(manifest: object, files: object, settings: InstallSettings | None = None) -> list[Diagnostic]:
    """The ``verify`` phase: shape, digest and revision claims, the quarantine record, credentials."""
    settings = settings or InstallSettings()
    out = _check_shape(manifest, files, settings)
    if out or not isinstance(manifest, dict) or not isinstance(files, dict):
        return out
    try:
        digest, revision = manifest_digest(manifest), revision_of(manifest, files)
    except PackContentError as error:
        return [Diagnostic(PHASE_VERIFY, "pack.schema", str(error))]
    provenance = manifest.get("provenance")
    recorded = provenance.get("digest") if isinstance(provenance, dict) else None
    if recorded is not None and recorded != digest:
        out.append(
            Diagnostic(
                PHASE_VERIFY,
                "pack.digest",
                "provenance.digest does not name the pack's content: it was edited after it was reviewed",
                "/provenance/digest",
            )
        )
    if settings.expected_revision is not None and settings.expected_revision != revision:
        out.append(
            Diagnostic(
                PHASE_VERIFY,
                "pack.revision",
                "the revision named for this install is not the revision of the content sent",
                "/revision",
            )
        )
    out.extend(_check_record(manifest))
    out.extend(_credential_findings(manifest, files))
    return out


def compile_install(
    manifest: object,
    files: object,
    tools: Sequence[ToolVisibility],
    settings: InstallSettings | None = None,
) -> PackCompilation:
    """Verify and compile a pack. Never raises for a bad pack: its problems are diagnostics."""
    settings = settings or InstallSettings()
    diagnostics = verify_pack(manifest, files, settings)
    if (
        not isinstance(manifest, dict)
        or not isinstance(files, dict)
        or any(item.rule == "pack.schema" for item in diagnostics)
    ):
        # Without a well-formed pack there is nothing to hash or compile.
        return PackCompilation(revision="", manifest_digest="", diagnostics=diagnostics)
    revision, digest = revision_of(manifest, files), manifest_digest(manifest)
    registry = {tool.name: tool for tool in tools}
    contents = _as_dict(manifest.get("contents"))
    granted: list[str] = []
    seen: set[str] = set()
    for index, name in enumerate(contents.get("tool_names", [])):
        pointer = f"/contents/tool_names/{index}"
        if not _TOOL.match(name):
            diagnostics.append(Diagnostic(PHASE_COMPILE, "pack.tool.unknown", "not a tool name", pointer))
            continue
        tool = registry.get(name)
        if name in seen:
            diagnostics.append(
                Diagnostic(
                    PHASE_COMPILE, "pack.tool.duplicate", "the tool is named twice", pointer, "warning"
                )
            )
            continue
        seen.add(name)
        if tool is None:
            diagnostics.append(
                Diagnostic(PHASE_COMPILE, "pack.tool.unknown", "the Tool Registry has no such tool", pointer)
            )
        elif not tool.model_visible:
            diagnostics.append(
                Diagnostic(
                    PHASE_COMPILE,
                    "pack.tool.host_only",
                    "the tool is not model-visible; a pack cannot make a tool visible to a model",
                    pointer,
                )
            )
        else:
            granted.append(name)
    evaluation = _as_dict(manifest.get("evaluation"))
    suite = contents.get("eval_suite_id")
    if suite is not None and evaluation.get("eval_suite_id") not in (None, suite):
        diagnostics.append(
            Diagnostic(
                PHASE_COMPILE,
                "pack.eval.suite",
                "the evaluation record is for another suite than the pack names",
                "/evaluation/eval_suite_id",
            )
        )
    # The CAP-005 gate, over the record the pack ships with.
    recorded = evaluation.get("result")
    gate = Qualification(
        passed=(str(suite),) if recorded == "pass" else (),
        failed=() if recorded == "pass" else (str(suite),),
        protected_failed=(),
    )
    if not gate.ok and not any(item.rule == "pack.quarantine" for item in diagnostics):
        diagnostics.append(
            Diagnostic(PHASE_COMPILE, "pack.eval", "the evaluation gate did not pass", "/evaluation/result")
        )
    resolution = {
        "tools": sorted(granted),
        "skills": list(contents.get("skill_version_ids", [])),
        "knowledge": list(contents.get("knowledge_ids", [])),
        "workflow_templates": len(contents.get("workflow_templates", [])),
        "connectors": list(contents.get("connector_requirements", [])),
        "widened": False,
    }
    return PackCompilation(
        revision=revision, manifest_digest=digest, diagnostics=diagnostics, resolution=resolution
    )
