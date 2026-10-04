"""Compile a recording's text projection into a skill proposal (ADP-003, DOMAIN.md §11.7 D).

A recording is *evidence of intent*, never a script to replay. The compiler therefore produces a
procedure that names applications by bundle id and controls by their accessibility attributes, turns every
value the person typed into an input slot the person must classify, derives preconditions, postconditions
and recovery, and prefers an equivalent Tool to driving pixels. It is deterministic and calls no model:
the same projection, tools and settings always give the same proposal.

Everything the recording says is untrusted data (§12 rule 7): window titles, control titles, recognised
screen text and narration are scanned with the deterministic injection heuristics, listed under
``trust.injection_suspected`` and never copied into instructions; a control title that reads as an
instruction is replaced in the manifest. The compiler never adds a tool or a capability that the registry's
own tool catalog does not give it, never holds a screen position, and ends in the same validator the server
runs, so a bug here cannot become a coordinate in a skill. It has no write path of any kind: it returns a
proposal and the Rust control plane decides what to keep.
"""

from __future__ import annotations

import json
import re
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any

from intelligence.skills.recording.proposal import (
    POINT_TEXT,
    PROCEDURE_SCHEMA_VERSION,
    PROPOSAL_SCHEMA_VERSION,
    Limits,
    ProposalError,
    ToolInfo,
    validate_proposal,
)
from intelligence.skills.recording.redact import RULE_SET, scan_text
from intelligence.trust.classifier import detect_injection


class RecordingCompileError(ValueError):
    """A recording the compiler refuses, with the DOMAIN.md §15 code and the rule."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


RULE_UNREDACTED = "recording.unredacted"
RULE_PROJECTION = "recording.projection"
RULE_BOUNDS = "recording.bounds"

_AX_KEYS = ("ax_role", "ax_subrole", "ax_title", "ax_identifier", "ax_description")
_TITLE_CAP = 60
_EXAMPLE_CAP = 80
_CONSEQUENTIAL = re.compile(
    r"\b(?:send|delete|remove|buy|purchase|pay|submit|publish|post|transfer|confirm|share|upload|erase|empty)\b",
    re.IGNORECASE,
)
_EMAIL = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")
_URL = re.compile(r"^https?://\S+$", re.IGNORECASE)
_NUMBER = re.compile(r"^-?\d+(?:[.,]\d+)?$")
_DATE = re.compile(r"^(?:\d{4}-\d{2}-\d{2}|\d{1,2}[/.]\d{1,2}[/.]\d{2,4})$")
_PLACEHOLDER = "[a control whose title was withheld]"

_RECOVERY_ANY = (
    "If a control is not found, observe again once; if it is still missing, stop and ask the person. "
    "Never repeat an action whose outcome is unknown: observe to find out what happened first."
)
_RECOVERY_CONSEQUENTIAL = (
    "This step may send, delete, pay or publish. If its outcome cannot be told, reconcile by observing the "
    "result before doing anything else; never repeat it blind."
)


@dataclass(frozen=True, slots=True)
class CompileSettings:
    """What the compiler needs of ``config/skill-recording.yaml``."""

    limits: Limits = field(default_factory=Limits)
    tool_paths: tuple[Mapping[str, Any], ...] = ()

    @classmethod
    def parse(cls, raw: str | Mapping[str, Any] | None) -> CompileSettings:
        if not raw:
            return cls()
        data: dict[str, Any] = json.loads(raw) if isinstance(raw, str) else dict(raw)
        proposal: dict[str, Any] = data.get("proposal") or {}
        defaults = Limits()
        return cls(
            limits=Limits(
                max_bytes=int(proposal.get("max_bytes", defaults.max_bytes)),
                max_steps=int(proposal.get("max_steps", defaults.max_steps)),
                max_slots=int(proposal.get("max_slots", defaults.max_slots)),
            ),
            tool_paths=tuple(data.get("tool_paths") or ()),
        )


def _walk_strings(value: object, pointer: str) -> Iterator[tuple[str, str]]:
    if isinstance(value, str):
        yield pointer, value
    elif isinstance(value, Mapping):
        for key, inner in value.items():
            yield from _walk_strings(inner, f"{pointer}/{key}")
    elif isinstance(value, list):
        for index, inner in enumerate(value):
            yield from _walk_strings(inner, f"{pointer}/{index}")


def assert_redacted(projection: Mapping[str, Any]) -> None:
    """The plane's gate: refuse a projection in which redaction would still remove something.

    Never names the matched text: only the categories and where.
    """
    categories: set[str] = set()
    pointers: list[str] = []
    for pointer, text in _walk_strings(projection, ""):
        found = scan_text(text)
        if found:
            categories.update(found)
            if len(pointers) < 20:
                pointers.append(pointer)
    if categories:
        raise RecordingCompileError(
            "RECORDING_UNREDACTED",
            RULE_UNREDACTED,
            f"the recording still holds material redaction removes ({sorted(categories)} at {pointers})",
        )


def _snake(label: str) -> str:
    words = re.findall(r"[A-Za-z0-9]+", label)
    return "_".join(word.lower() for word in words)[:40]


def _safe_title(text: str, flagged: bool) -> str:
    """A control title as it may appear in a manifest: one short line, nothing that reads as an order."""
    if flagged:
        return _PLACEHOLDER
    line = " ".join(text.split())
    line = line.replace('"', "'")
    return line[:_TITLE_CAP]


def _slot_type(text: str) -> str:
    stripped = text.strip()
    if _EMAIL.match(stripped):
        return "email"
    if _URL.match(stripped):
        return "url"
    if _DATE.match(stripped):
        return "date"
    if _NUMBER.match(stripped):
        return "number"
    return "text"


@dataclass(slots=True)
class _Action:
    seq: int
    kind: str
    bundle_id: str
    app_name: str
    target: dict[str, str]
    unresolved: bool
    text: str | None
    key: str | None
    secure: bool


def _parse_actions(projection: Mapping[str, Any]) -> list[_Action]:
    raw = projection.get("actions")
    if not isinstance(raw, list):
        raise RecordingCompileError(
            "VALIDATION_SCHEMA", RULE_PROJECTION, "the projection needs an actions list"
        )
    actions: list[_Action] = []
    for entry in raw:
        if not isinstance(entry, Mapping):
            raise RecordingCompileError("VALIDATION_SCHEMA", RULE_PROJECTION, "an action must be an object")
        app: Mapping[str, Any] = entry["app"] if isinstance(entry.get("app"), Mapping) else {}
        target: Mapping[str, Any] = entry["target"] if isinstance(entry.get("target"), Mapping) else {}
        bundle_id = app.get("bundle_id")
        if not isinstance(bundle_id, str) or not bundle_id:
            raise RecordingCompileError(
                "VALIDATION_SCHEMA", RULE_PROJECTION, "an action needs its app's bundle id"
            )
        actions.append(
            _Action(
                seq=int(entry.get("seq", len(actions) + 1)),
                kind=str(entry.get("kind", "")),
                bundle_id=bundle_id,
                app_name=str(app.get("name") or bundle_id),
                target={k: str(target[k]) for k in _AX_KEYS if isinstance(target.get(k), str) and target[k]},
                unresolved=bool(entry.get("target_unresolved")),
                text=entry["text"] if isinstance(entry.get("text"), str) else None,
                key=entry["key"] if isinstance(entry.get("key"), str) else None,
                secure=bool(entry.get("secure")),
            )
        )
    actions.sort(key=lambda action: action.seq)
    return actions


def _flag_untrusted(projection: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Every untrusted string that reads like an instruction, by where it was (never its text)."""
    flagged: list[dict[str, Any]] = []
    for section in ("actions", "transcript", "screen_text"):
        for pointer, text in _walk_strings(projection.get(section), f"/{section}"):
            if pointer.endswith(("/at", "/kind", "/seq", "/bundle_id")):
                continue
            patterns = detect_injection(text)
            if patterns:
                flagged.append({"pointer": pointer, "patterns": list(patterns)})
    return flagged[:50]


class _Builder:
    def __init__(
        self,
        recording_id: str,
        actions: list[_Action],
        tools: Sequence[ToolInfo],
        settings: CompileSettings,
        flagged_pointers: set[str],
    ) -> None:
        self.recording_id = recording_id
        self.actions = actions
        self.tools = {tool.name: tool for tool in tools}
        self.settings = settings
        self.flagged = flagged_pointers
        self.slots: list[dict[str, Any]] = []
        self.steps: list[dict[str, Any]] = []
        self.questions: list[dict[str, str]] = []
        self.tool_needs: set[str] = set()
        self.apps: dict[str, str] = {}
        self.postconditions: list[dict[str, Any]] = []
        self.recovery: list[dict[str, str]] = []
        self.instruction_lines: list[str] = []

    # ---- tool preference -------------------------------------------------------------------------
    def tool_path(self, bundle_id: str) -> list[str]:
        """Offered tools that do this application's work, when the configuration says there are some."""
        for path in self.settings.tool_paths:
            if bundle_id not in (path.get("bundle_ids") or ()):
                continue
            if path.get("tools"):
                offered = sorted(name for name in path["tools"] if name in self.tools)
            elif path.get("tool_prefix"):
                offered = sorted(name for name in self.tools if name.startswith(str(path["tool_prefix"])))
            else:
                offered = []
            if offered:
                return offered
        return []

    # ---- helpers ---------------------------------------------------------------------------------
    def _flagged(self, action: _Action, key: str) -> bool:
        """A control attribute that reads as an instruction, or as a screen position, is not carried."""
        value = action.target.get(key, "")
        return f"/actions/{self.actions.index(action)}/target/{key}" in self.flagged or any(
            pattern.search(value) for pattern in POINT_TEXT
        )

    def _title(self, action: _Action, key: str) -> str:
        return _safe_title(action.target.get(key, ""), self._flagged(action, key))

    def _label(self, action: _Action) -> str:
        for key in ("ax_title", "ax_description", "ax_identifier"):
            if key in action.target:
                return self._title(action, key)
        return action.target.get("ax_role", "the control")

    def _ui(self, action: _Action, verb: str) -> dict[str, Any]:
        ui: dict[str, Any] = {"bundle_id": action.bundle_id, "action": verb}
        for key in _AX_KEYS:
            if key in action.target and not self._flagged(action, key):
                ui[key] = action.target[key][:200]
        return ui

    def _need(self, tool: str, step: dict[str, Any]) -> None:
        if tool in self.tools:
            self.tool_needs.add(tool)
        else:
            step["needs_review"] = True

    def _slot_for(self, action: _Action) -> dict[str, Any] | None:
        text = action.text
        if action.secure or text is None or not text.strip():
            return None
        number = len(self.slots) + 1
        label = self._label(action)
        name = _snake(label) or f"value_{number}"
        taken = {slot["name"] for slot in self.slots}
        if name in taken:
            name = f"{name}_{number}"
        control = {
            key: action.target[key][:200]
            for key in _AX_KEYS
            if key in action.target and not self._flagged(action, key)
        }
        slot: dict[str, Any] = {
            "slot_id": f"slot_{number}",
            "name": name,
            "description": f"Value entered in {label} ({action.app_name})"[:200],
            "type": _slot_type(text),
            "example": text[:_EXAMPLE_CAP],
            "classification": None,
            "needs_user_decision": True,
            "control": control,
        }
        self.slots.append(slot)
        self.questions.append(
            {
                "slot_id": slot["slot_id"],
                "question": (
                    f"The recording typed a value into {label}. Is it a constant of this workflow, "
                    "or something to provide each time?"
                ),
            }
        )
        return slot

    # ---- steps -----------------------------------------------------------------------------------
    def build(self) -> None:
        index = 0
        while index < len(self.actions):
            action = self.actions[index]
            self.apps.setdefault(action.bundle_id, action.app_name)
            preferred = self.tool_path(action.bundle_id)
            if preferred:
                index = self._tool_step(index, preferred)
                continue
            self._ui_step(action)
            index += 1

    def _tool_step(self, start: int, preferred: list[str]) -> int:
        first = self.actions[start]
        end = start
        while end + 1 < len(self.actions) and self.actions[end + 1].bundle_id == first.bundle_id:
            end += 1
        group = self.actions[start : end + 1]
        slot_ids = []
        for action in group:
            if action.kind in ("type", "set_value"):
                slot = self._slot_for(action)
                if slot:
                    slot_ids.append(slot["slot_id"])
        number = len(self.steps) + 1
        step: dict[str, Any] = {
            "id": f"s{number}",
            "intent": (
                f"Do the work of this part of the demonstration in {first.app_name} with "
                f"{', '.join(preferred)} instead of driving its screen"
            ),
            "path": "tool",
            "tools": preferred,
            "provenance": {"action_seqs": [action.seq for action in group][:200]},
        }
        if slot_ids:
            step["slot_ids"] = slot_ids
        self.tool_needs.update(preferred)
        self.steps.append(step)
        self.instruction_lines.append(
            f"{number}. In {first.app_name}, use {', '.join(preferred)} rather than operating its screen."
        )
        return end + 1

    def _ui_step(self, action: _Action) -> None:
        number = len(self.steps) + 1
        step: dict[str, Any] = {"id": f"s{number}", "path": "ui", "provenance": {"action_seqs": [action.seq]}}
        label = self._label(action)
        where = f'"{label}" ({action.target["ax_role"]})' if "ax_role" in action.target else f'"{label}"'
        kind = action.kind
        if kind == "app_switch":
            step["ui"] = {"bundle_id": action.bundle_id, "action": "activate_app"}
            step["intent"] = f"Bring {action.app_name} to the front"
            line = f"Bring {action.app_name} to the front."
            self._need("computer.read", step)
        elif kind in ("type", "set_value"):
            slot = self._slot_for(action)
            step["ui"] = self._ui(action, "set_value")
            if slot is None:
                # A secret or an empty entry: the skill never types it. The person does, in the control.
                step["ui"]["action"] = "focus"
                step["intent"] = (
                    f"Focus {where} in {action.app_name} and let the person enter the value themselves"
                )
                step["needs_review"] = True
                line = f"Focus {where} in {action.app_name}; the person enters the value themselves."
                self._need("computer.read", step)
            else:
                step["ui"]["slot_id"] = slot["slot_id"]
                step["intent"] = f"Enter the value of {slot['name']} into {where} in {action.app_name}"
                line = f"Set {where} in {action.app_name} to the value of {slot['name']}."
                self._need("computer.read", step)
                self._need("computer.type", step)
                self.postconditions.append(
                    {
                        "kind": "value_present",
                        "slot_id": slot["slot_id"],
                        "bundle_id": action.bundle_id,
                        "description": f"{where} in {action.app_name} holds the value of {slot['name']}",
                        **{
                            k: action.target[k][:200]
                            for k in ("ax_role", "ax_title", "ax_identifier")
                            if k in action.target and not self._flagged(action, k)
                        },
                    }
                )
        elif kind == "key":
            step["ui"] = {"bundle_id": action.bundle_id, "action": "key", "key": (action.key or "")[:64]}
            step["intent"] = f"Press the key chord {action.key or ''} in {action.app_name}"
            line = f"Press {action.key or 'the key'} in {action.app_name}."
            self._need("computer.system_key", step)
        elif kind == "scroll":
            step["ui"] = self._ui(action, "scroll")
            step["intent"] = f"Scroll {where} in {action.app_name}"
            line = f"Scroll {where} in {action.app_name}."
            self._need("computer.system_key", step)
        elif kind in ("focus",):
            step["ui"] = self._ui(action, "focus")
            step["intent"] = f"Focus {where} in {action.app_name}"
            line = f"Focus {where} in {action.app_name}."
            self._need("computer.read", step)
        elif kind == "menu_select":
            step["ui"] = self._ui(action, "select")
            step["intent"] = f"Choose {where} from the menu in {action.app_name}"
            line = f"Choose {where} from the menu in {action.app_name}."
            self._need("computer.click", step)
        else:
            verb = "open_menu" if kind == "right_click" else "press"
            step["ui"] = self._ui(action, verb)
            if action.unresolved or not action.target:
                step["needs_review"] = True
                step["intent"] = (
                    f"Press the control the person used in {action.app_name}: the recorder could not "
                    "identify it, so observe and ask which control it was"
                )
                line = (
                    f"Press the control the person used in {action.app_name} (not identified by the "
                    "recording: observe, then ask which one)."
                )
                self.questions.append(
                    {
                        "step_id": step["id"],
                        "question": (
                            f"The recorder could not identify the control pressed in {action.app_name}. "
                            "Which control is it?"
                        ),
                    }
                )
            else:
                noun = "Open the context menu of" if verb == "open_menu" else "Press"
                step["intent"] = f"{noun} {where} in {action.app_name}"
                line = f"{noun} {where} in {action.app_name}."
            self._need("computer.read", step)
            self._need("computer.click", step)
            if _CONSEQUENTIAL.search(label):
                self.recovery.append(
                    {
                        "on": step["id"],
                        "action": "reconcile_by_observation",
                        "guidance": _RECOVERY_CONSEQUENTIAL,
                    }
                )
        self.steps.append(step)
        self.instruction_lines.append(f"{number}. {line}")

    # ---- assembly --------------------------------------------------------------------------------
    def proposal(self, injection: list[dict[str, Any]]) -> dict[str, Any]:
        if not self.steps:
            raise RecordingCompileError(
                "VALIDATION_SCHEMA",
                RULE_PROJECTION,
                "the recording holds no action to compile into a procedure",
            )
        if len(self.steps) > self.settings.limits.max_steps:
            raise RecordingCompileError(
                "VALIDATION_BOUNDS",
                RULE_BOUNDS,
                f"{len(self.steps)} steps; the limit is {self.settings.limits.max_steps}",
            )
        apps = [{"bundle_id": bundle_id, "name": name[:200]} for bundle_id, name in self.apps.items()]
        preconditions: list[dict[str, Any]] = [
            {
                "kind": "app_installed",
                "bundle_id": app["bundle_id"],
                "description": f"{app['name']} is installed",
            }
            for app in apps
        ]
        preconditions.extend(
            {
                "kind": "slot_provided",
                "slot_id": slot["slot_id"],
                "description": f"A value for {slot['name']} is available (provided, or fixed by the person)",
            }
            for slot in self.slots
        )
        postconditions = list(self.postconditions)
        postconditions.append(
            {
                "kind": "evidence_required",
                "description": (
                    "Completion needs observed evidence of the final state; the run's own claim is not enough"
                ),
            }
        )
        recovery = [{"on": "any", "action": "reobserve_then_ask", "guidance": _RECOVERY_ANY}, *self.recovery]
        tool_needs = sorted(self.tool_needs)
        capability_needs = sorted({c for name in tool_needs for c in self.tools[name].effect_classes})
        slot_names = ", ".join(slot["name"] for slot in self.slots) or "none"
        apps_line = ", ".join(f"{app['name']} ({app['bundle_id']})" for app in apps)
        instructions = "\n".join(
            [
                "Recorded workflow, compiled from a person's demonstration. The recording is evidence of "
                "intent, not a script: find each control by its accessibility attributes in a fresh "
                "observation, never by a remembered position.",
                f"Applications: {apps_line}.",
                f"Inputs: {slot_names}. Ask the person for an input before the first step that uses it, "
                "unless the person fixed it as a constant.",
                "Steps:",
                *self.instruction_lines,
                "Every action is classified and approved like any other action; this skill grants nothing. "
                "Verify the postconditions with an observation and do not claim completion without evidence.",
            ]
        )
        examples = []
        for app in apps[:3]:
            examples.append(f"use {app['name']}")
        for step in self.steps[:2]:
            intent = step["intent"].split(" in ")[0]
            if len(intent) <= 80 and intent not in examples:
                examples.append(intent)
        manifest = {
            "instructions": instructions,
            "examples": examples,
            "tool_needs": tool_needs,
            "capability_needs": capability_needs,
            "compatibility": ["v8.1", "macos"],
            "recovery_guidance": _RECOVERY_ANY,
            "procedure": {
                "schema_version": PROCEDURE_SCHEMA_VERSION,
                "platform": "macos",
                "apps": apps,
                "inputs": [
                    {
                        "slot_id": slot["slot_id"],
                        "name": slot["name"],
                        "description": slot["description"],
                        "type": slot["type"],
                    }
                    for slot in self.slots
                ],
                "preconditions": preconditions,
                "steps": self.steps,
                "postconditions": postconditions,
                "recovery": recovery,
            },
        }
        return {
            "schema_version": PROPOSAL_SCHEMA_VERSION,
            "trust": {
                "source": "recording",
                "level": "UNTRUSTED_EXTERNAL",
                "recording_id": self.recording_id,
                "injection_suspected": injection,
            },
            "manifest": manifest,
            "slots": self.slots,
            "questions": self.questions,
        }


def compile_recording(
    *,
    recording_id: str,
    projection: Mapping[str, Any],
    tools: Sequence[ToolInfo],
    settings: CompileSettings | None = None,
) -> dict[str, Any]:
    """Compile one recording's text projection into a skill proposal.

    Raises :class:`RecordingCompileError` for a projection that is unredacted or unusable and for a
    proposal the validator refuses (a bug here, never something to ship).
    """
    settings = settings or CompileSettings()
    redaction = projection.get("redaction")
    if not isinstance(redaction, Mapping) or redaction.get("rule_set") != RULE_SET:
        raise RecordingCompileError(
            "RECORDING_UNREDACTED",
            RULE_UNREDACTED,
            f"the projection was not redacted under rule set {RULE_SET}",
        )
    assert_redacted(projection)
    actions = _parse_actions(projection)
    injection = _flag_untrusted(projection)
    flagged_pointers = {entry["pointer"] for entry in injection}
    builder = _Builder(recording_id, actions, tools, settings, flagged_pointers)
    builder.build()
    proposal = builder.proposal(injection)
    try:
        validate_proposal(proposal, recording_id=recording_id, tools=tools, limits=settings.limits)
    except ProposalError as refused:
        raise RecordingCompileError("RECORDING_PROPOSAL_REJECTED", refused.rule, refused.detail) from refused
    return proposal
