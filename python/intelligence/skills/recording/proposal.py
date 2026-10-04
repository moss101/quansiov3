"""The shape of a skill proposal compiled from a recording, and the validator that enforces it
(DOMAIN.md §11.7 D).

A proposal is data in a closed vocabulary. The validator is the compiler's own last gate and is mirrored by
the server (``crates/server/.../recording/proposal.rs``), which does not trust the plane: it fails a
proposal that

* names a key or step shape outside the closed vocabulary (so a grant, a daemon or a free-form field cannot
  ride along);
* holds a screen coordinate or an element index, by key or by value, anywhere a skill is built from;
* names a tool the Tool Registry does not offer, or an effect class the named tools do not need;
* classifies a value as a constant or an input (only the person does that);
* holds anything redaction removes.
"""

from __future__ import annotations

import json
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from intelligence.skills.recording.redact import scan_text

PROPOSAL_SCHEMA_VERSION = "v1"
PROCEDURE_SCHEMA_VERSION = "v1"

#: Keys of a skill manifest (DOMAIN.md §11.5) plus ``procedure`` (§11.7 D).
MANIFEST_KEYS = frozenset(
    {
        "instructions",
        "examples",
        "tool_needs",
        "capability_needs",
        "eval_suite_id",
        "compatibility",
        "recovery_guidance",
        "procedure",
    }
)
SLOT_TYPES = ("text", "number", "choice", "file", "date", "email", "url")
PRECONDITION_KINDS = ("app_installed", "app_running", "slot_provided", "control_present")
POSTCONDITION_KINDS = ("app_frontmost", "value_present", "control_present", "evidence_required")
UI_ACTIONS = ("press", "set_value", "focus", "select", "open_menu", "key", "scroll", "activate_app")
RECOVERY_ACTIONS = ("reobserve_then_ask", "stop_and_ask", "reconcile_by_observation")

#: Keys that name where something was on the screen, or which one of several it was.
FORBIDDEN_KEYS = frozenset(
    {
        "x",
        "y",
        "point",
        "position",
        "coordinate",
        "coordinates",
        "frame",
        "bounds",
        "rect",
        "origin",
        "index",
        "element_index",
        "ref",
        "nth",
        "offset",
        "idx",
        "screen_x",
        "screen_y",
        "pixel",
    }
)
POINT_TEXT = tuple(
    re.compile(pattern)
    for pattern in (
        r"\(\s*-?\d+(?:\.\d+)?\s*,\s*-?\d+(?:\.\d+)?\s*\)",
        r"(?i)\b[xy]\s*[=:]\s*-?\d+",
        r"\[\s*\d+\s*\]",
        r"(?i)\b(?:index|nth|element)\s*#?\s*\d+\b",
    )
)


class ProposalError(ValueError):
    """A proposal that broke a rule, naming it (the server's ``details.rule``)."""

    def __init__(self, rule: str, detail: str) -> None:
        super().__init__(f"{rule}: {detail}")
        self.rule = rule
        self.detail = detail


@dataclass(frozen=True, slots=True)
class ToolInfo:
    """One tool the registry offers, as the compiler is told about it."""

    name: str
    description: str
    effect_classes: tuple[str, ...]

    @classmethod
    def parse(cls, raw: str | Mapping[str, object]) -> ToolInfo:
        data: dict[str, Any] = json.loads(raw) if isinstance(raw, str) else dict(raw)
        classes: list[Any] = list(data.get("effect_classes") or [])
        return cls(
            name=str(data["name"]),
            description=str(data.get("description", "")),
            effect_classes=tuple(str(c) for c in classes),
        )


@dataclass(frozen=True, slots=True)
class Limits:
    """The proposal limits from ``config/skill-recording.yaml``."""

    max_bytes: int = 262_144
    max_steps: int = 200
    max_slots: int = 64


def _closed(value: object, what: str, keys: Sequence[str] | frozenset[str], rule: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ProposalError(rule, f"{what} must be an object")
    unknown = [key for key in value if key not in keys]
    if unknown:
        raise ProposalError(rule, f"{what} has a field outside the closed vocabulary: `{unknown[0]}`")
    return value


def _text(mapping: Mapping[str, Any], key: str, what: str) -> str:
    value = mapping.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ProposalError("proposal.procedure", f"{what}.{key} must be non-empty text")
    return value


def _list(mapping: Mapping[str, Any], key: str, what: str) -> list[Any]:
    value = mapping.get(key)
    if not isinstance(value, list):
        raise ProposalError("proposal.procedure", f"{what}.{key} must be a list")
    return value


def _strings(mapping: Mapping[str, Any], key: str) -> list[str]:
    value = mapping.get(key)
    return [item for item in value if isinstance(item, str)] if isinstance(value, list) else []


def find_coordinates(value: object, pointer: str, *, strings: bool) -> str | None:
    """Where a screen position or element index sits in ``value``, or ``None``."""
    if isinstance(value, Mapping):
        for key, inner in value.items():
            child = f"{pointer}/{key}"
            if str(key).lower() in FORBIDDEN_KEYS:
                return child
            found = find_coordinates(inner, child, strings=strings)
            if found:
                return found
    elif isinstance(value, list):
        for index, inner in enumerate(value):
            found = find_coordinates(inner, f"{pointer}/{index}", strings=strings)
            if found:
                return found
    elif strings and isinstance(value, str) and any(p.search(value) for p in POINT_TEXT):
        return pointer
    return None


def _find_redactable(value: object, pointer: str) -> str | None:
    if isinstance(value, Mapping):
        for key, inner in value.items():
            found = _find_redactable(inner, f"{pointer}/{key}")
            if found:
                return found
    elif isinstance(value, list):
        for index, inner in enumerate(value):
            found = _find_redactable(inner, f"{pointer}/{index}")
            if found:
                return found
    elif isinstance(value, str) and scan_text(value):
        return pointer
    return None


def validate_proposal(
    proposal: Mapping[str, Any],
    *,
    recording_id: str,
    tools: Sequence[ToolInfo],
    limits: Limits = Limits(),  # noqa: B008 - an immutable value
) -> None:
    """Raise :class:`ProposalError` naming the first rule the proposal breaks."""
    size = len(json.dumps(proposal, separators=(",", ":")))
    if size > limits.max_bytes:
        raise ProposalError("proposal.size", f"{size} bytes; the limit is {limits.max_bytes}")
    root = _closed(
        proposal,
        "the proposal",
        ("schema_version", "trust", "manifest", "slots", "questions"),
        "proposal.shape",
    )
    if root.get("schema_version") != PROPOSAL_SCHEMA_VERSION:
        raise ProposalError("proposal.shape", "schema_version must be v1")
    trust = _closed(
        root.get("trust"),
        "trust",
        ("source", "level", "recording_id", "injection_suspected"),
        "proposal.trust",
    )
    if (
        trust.get("source") != "recording"
        or trust.get("level") != "UNTRUSTED_EXTERNAL"
        or trust.get("recording_id") != recording_id
    ):
        raise ProposalError(
            "proposal.trust",
            "a proposal carries its origin: recording, UNTRUSTED_EXTERNAL, and this recording's id",
        )
    manifest = _closed(root.get("manifest"), "the manifest", MANIFEST_KEYS, "proposal.manifest_vocabulary")
    if "eval_suite_id" in manifest:
        raise ProposalError(
            "proposal.manifest_vocabulary", "a proposal cannot choose its own evaluation suite"
        )
    instructions = manifest.get("instructions")
    if not isinstance(instructions, str) or not instructions.strip():
        raise ProposalError("proposal.manifest_vocabulary", "instructions are required")

    known = {tool.name: tool for tool in tools}
    tool_needs = _strings(manifest, "tool_needs")
    allowed_classes: set[str] = set()
    for name in tool_needs:
        if name not in known:
            raise ProposalError("proposal.tools", f"`{name}` is not a tool the registry offers")
        allowed_classes.update(known[name].effect_classes)
    for effect_class in _strings(manifest, "capability_needs"):
        if effect_class not in allowed_classes:
            raise ProposalError(
                "proposal.capabilities", f"`{effect_class}` is not an effect class of a tool the skill needs"
            )

    procedure = _closed(
        manifest.get("procedure"),
        "procedure",
        (
            "schema_version",
            "platform",
            "apps",
            "inputs",
            "preconditions",
            "steps",
            "postconditions",
            "recovery",
        ),
        "proposal.procedure",
    )
    if procedure.get("schema_version") != PROCEDURE_SCHEMA_VERSION:
        raise ProposalError("proposal.procedure", "procedure.schema_version must be v1")
    for entry in _list(procedure, "apps", "procedure"):
        _text(_closed(entry, "an app", ("bundle_id", "name"), "proposal.procedure"), "bundle_id", "app")

    slots = root.get("slots")
    if not isinstance(slots, list):
        raise ProposalError("proposal.slots", "slots must be a list")
    if len(slots) > limits.max_slots:
        raise ProposalError("proposal.slots", f"{len(slots)} slots; the limit is {limits.max_slots}")
    slot_ids: set[str] = set()
    for slot in slots:
        item = _closed(
            slot,
            "a slot",
            (
                "slot_id",
                "name",
                "description",
                "type",
                "example",
                "classification",
                "needs_user_decision",
                "control",
            ),
            "proposal.slots",
        )
        slot_id = item.get("slot_id")
        if not isinstance(slot_id, str):
            raise ProposalError("proposal.slots", "a slot needs a slot_id")
        if slot_id in slot_ids:
            raise ProposalError("proposal.slots", f"slot_id `{slot_id}` is used twice")
        slot_ids.add(slot_id)
        if item.get("classification") is not None:
            raise ProposalError(
                "proposal.unclassified",
                "a proposal cannot decide whether a value is a constant or an input; the person does",
            )
        if item.get("type") not in SLOT_TYPES:
            raise ProposalError("proposal.slots", f"`{item.get('type')}` is not a slot type")

    declared: set[str] = set()
    for entry in _list(procedure, "inputs", "procedure"):
        item = _closed(
            entry,
            "an input",
            ("slot_id", "name", "description", "type", "classification", "constant_value"),
            "proposal.procedure",
        )
        slot_id = _text(item, "slot_id", "input")
        if slot_id not in slot_ids:
            raise ProposalError("proposal.slots", f"input `{slot_id}` has no slot")
        if item.get("classification") is not None or "constant_value" in item:
            raise ProposalError("proposal.unclassified", "an input is unclassified until the person decides")
        declared.add(slot_id)
    if declared != slot_ids:
        raise ProposalError(
            "proposal.slots", "every slot is an input of the procedure, and no input lacks a slot"
        )
    for entry in _list(procedure, "preconditions", "procedure"):
        pre = _closed(
            entry, "a precondition", ("kind", "bundle_id", "slot_id", "description"), "proposal.procedure"
        )
        kind = _text(pre, "kind", "precondition")
        if kind not in PRECONDITION_KINDS:
            raise ProposalError("proposal.procedure", f"`{kind}` is not a precondition kind")
        slot = pre.get("slot_id")
        if isinstance(slot, str) and slot not in slot_ids:
            raise ProposalError("proposal.slots", f"precondition names unknown slot `{slot}`")

    steps = _list(procedure, "steps", "procedure")
    if not steps or len(steps) > limits.max_steps:
        raise ProposalError("proposal.procedure", f"{len(steps)} steps; allowed 1..={limits.max_steps}")
    step_ids: set[str] = set()
    for entry in steps:
        step = _closed(
            entry,
            "a step",
            ("id", "intent", "path", "tools", "slot_ids", "ui", "provenance", "needs_review"),
            "proposal.procedure",
        )
        step_id = _text(step, "id", "step")
        if step_id in step_ids:
            raise ProposalError("proposal.procedure", f"step id `{step_id}` is used twice")
        step_ids.add(step_id)
        _text(step, "intent", "step")
        for slot in _strings(step, "slot_ids"):
            if slot not in slot_ids:
                raise ProposalError("proposal.slots", f"step uses unknown slot `{slot}`")
        path = _text(step, "path", "step")
        if path == "tool":
            named = _strings(step, "tools")
            if not named or "ui" in step:
                raise ProposalError("proposal.procedure", "a tool step names tools and no ui")
            for tool in named:
                if tool not in tool_needs:
                    raise ProposalError(
                        "proposal.tools", f"step uses `{tool}` which the skill does not declare it needs"
                    )
        elif path == "ui":
            if "tools" in step:
                raise ProposalError("proposal.procedure", "a ui step names a control, not tools")
            ui = _closed(
                step.get("ui"),
                "a ui target",
                (
                    "bundle_id",
                    "ax_role",
                    "ax_subrole",
                    "ax_title",
                    "ax_identifier",
                    "ax_description",
                    "action",
                    "slot_id",
                    "key",
                ),
                "proposal.procedure",
            )
            _text(ui, "bundle_id", "ui")
            action = _text(ui, "action", "ui")
            if action not in UI_ACTIONS:
                raise ProposalError("proposal.procedure", f"`{action}` is not a ui action")
            slot = ui.get("slot_id")
            if isinstance(slot, str) and slot not in slot_ids:
                raise ProposalError("proposal.slots", f"step uses unknown slot `{slot}`")
        else:
            raise ProposalError("proposal.procedure", f"`{path}` is not a step path")
        if "provenance" in step:
            _closed(step["provenance"], "provenance", ("action_seqs",), "proposal.procedure")
    for entry in _list(procedure, "postconditions", "procedure"):
        post = _closed(
            entry,
            "a postcondition",
            ("kind", "description", "slot_id", "bundle_id", "ax_role", "ax_title", "ax_identifier"),
            "proposal.procedure",
        )
        kind = _text(post, "kind", "postcondition")
        if kind not in POSTCONDITION_KINDS:
            raise ProposalError("proposal.procedure", f"`{kind}` is not a postcondition kind")
    recovery = _list(procedure, "recovery", "procedure")
    if not recovery:
        raise ProposalError("proposal.procedure", "a procedure carries recovery")
    for entry in recovery:
        item = _closed(entry, "a recovery", ("on", "action", "guidance"), "proposal.procedure")
        _text(item, "on", "recovery")
        action = _text(item, "action", "recovery")
        if action not in RECOVERY_ACTIONS:
            raise ProposalError(
                "proposal.procedure", f"`{action}` is not a recovery action; none repeats an action blind"
            )

    for index, question in enumerate(root.get("questions") or []):
        _closed(question, f"questions[{index}]", ("slot_id", "step_id", "question"), "proposal.shape")

    found = find_coordinates(manifest, "/manifest", strings=True)
    if found:
        raise ProposalError("proposal.no_coordinates", f"a screen position or element index at {found}")
    for index, slot in enumerate(slots):
        found = find_coordinates(slot.get("control"), f"/slots/{index}/control", strings=False)
        if found:
            raise ProposalError("proposal.no_coordinates", f"a screen position or element index at {found}")
    redactable = _find_redactable(proposal, "")
    if redactable:
        raise ProposalError("proposal.redaction", f"text a redaction rule removes at {redactable}")
