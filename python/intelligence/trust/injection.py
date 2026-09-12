"""Deterministic injection heuristics (INT-012 build item 5).

These heuristics are deliberately **not** an LLM: they are patterns that run in the request path,
produce the same answer twice, and can be explained to a reviewer. A suspected segment is tagged,
**truncated to an evidence reference** in the context that reaches a model, and surfaced so a person
can look at it — being suspected is not the same as being wrong, so nothing is dropped silently.
"""

from __future__ import annotations

import re
from collections.abc import Sequence
from dataclasses import dataclass

#: Rule ids, recorded on every tag so a decision can be explained.
RULE_OVERRIDE = "injection.instruction_override"
RULE_ROLE_PLAY = "injection.role_play"
RULE_IMPERATIVE_TOOL = "injection.imperative_tool_call"
RULE_EXFILTRATION = "injection.exfiltration_url"
RULE_ENCODED_BLOB = "injection.encoded_blob"
RULE_HIDDEN_TEXT = "injection.hidden_text"

#: The deterministic patterns, each explaining itself.
PATTERNS: tuple[tuple[str, re.Pattern[str], str], ...] = (
    (
        RULE_OVERRIDE,
        re.compile(
            r"\b(ignore|disregard|forget)\b[^.]{0,40}\b(previous|prior|above|earlier|all)\b"
            r"[^.]{0,20}\b(instruction|prompt|rule|message)s?\b",
            re.IGNORECASE,
        ),
        "text tells the reader to ignore earlier instructions",
    ),
    (
        RULE_ROLE_PLAY,
        re.compile(
            r"\b(you are now|from now on you|act as|pretend to be|new (system )?prompt)\b",
            re.IGNORECASE,
        ),
        "text tries to reassign the reader's role",
    ),
    (
        RULE_IMPERATIVE_TOOL,
        # The tool noun must follow the verb directly (an optional article between them): "run the
        # shell tool" is an instruction, while "run the project's own test command" is prose about
        # running something. A loose window here flags ordinary documentation.
        re.compile(
            r"\b(run|execute|call|invoke|use)\s+(?:the\s+|a\s+|your\s+)?"
            r"(?:tool|tools|command|shell|terminal|function|api|browser)\b",
            re.IGNORECASE,
        ),
        "text instructs a tool call",
    ),
    (
        RULE_EXFILTRATION,
        re.compile(
            r"(https?://[^\s]*(?:token|secret|key|password|credential)[^\s]*"
            r"|\b(send|post|upload|exfiltrate|forward)\b[^.]{0,40}\b(to|at)\b[^.]{0,20}https?://)",
            re.IGNORECASE,
        ),
        "text asks for content to be sent to an external destination",
    ),
    (
        RULE_ENCODED_BLOB,
        re.compile(r"\b[A-Za-z0-9+/]{120,}={0,2}\b"),
        "text carries a long encoded blob",
    ),
    (
        RULE_HIDDEN_TEXT,
        re.compile(
            r"(<!--[^>]{0,200}(?:ignore|instruction|prompt)[^>]{0,200}-->|\u200b|\u2060)", re.IGNORECASE
        ),
        "text hides instructions in markup or invisible characters",
    ),
)


class InjectionError(ValueError):
    """A refused injection assessment."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class SuspectedSegment:
    """A segment the heuristics suspect, with the rules that fired and its evidence reference."""

    segment_id: str
    source: str
    rules: tuple[str, ...]
    reasons: tuple[str, ...]
    evidence_ref: str

    def surface(self) -> str:
        """One line a UI can show without reproducing the suspect text."""
        return f"{self.segment_id} ({self.source}): {', '.join(self.rules)} -> {self.evidence_ref}"


@dataclass(frozen=True, slots=True)
class Assessment:
    """What the heuristics decided about a segment."""

    segment_id: str
    suspected: bool
    rules: tuple[str, ...] = ()
    reasons: tuple[str, ...] = ()
    evidence_ref: str | None = None
    rendered_text: str = ""

    @property
    def evidence_reference(self) -> str:
        """The reference the context carries instead of the suspect text."""
        return self.evidence_ref or f"evidence://segment/{self.segment_id}"


def assess_segment(segment: object) -> Assessment:
    """Assess one labelled segment deterministically.

    A suspected segment's context text becomes its evidence reference: the model sees *that*
    something from that source was retrieved and can cite it, but not the instruction itself.
    """
    text = getattr(segment, "text", "")
    segment_id = getattr(segment, "segment_id", "segment")
    fired: list[str] = []
    reasons: list[str] = []
    for rule_id, pattern, reason in PATTERNS:
        if pattern.search(text):
            fired.append(rule_id)
            reasons.append(reason)
    if not fired:
        return Assessment(segment_id=segment_id, suspected=False, rendered_text=text)
    return Assessment(
        segment_id=segment_id,
        suspected=True,
        rules=tuple(fired),
        reasons=tuple(reasons),
        evidence_ref=f"evidence://segment/{segment_id}",
        rendered_text=f"[suspected injection: {', '.join(fired)}] evidence://segment/{segment_id}",
    )


def assess_all(segments: list[object]) -> tuple[Assessment, ...]:
    """Assess every segment, in input order (a pure function of the input)."""
    return tuple(assess_segment(segment) for segment in segments)


def surface_suspected(
    pairs: Sequence[tuple[Assessment, object]],
) -> tuple[SuspectedSegment, ...]:
    """The suspected segments, ready to surface, paired with the segments they came from.

    A caller keeps its own segments (this module never stores them), so it passes the pairs it
    assessed; the result carries the rules, the reasons and the evidence reference a UI shows.
    """
    return tuple(
        SuspectedSegment(
            segment_id=assessment.segment_id,
            source=str(getattr(segment, "source", "unknown")),
            rules=assessment.rules,
            reasons=assessment.reasons,
            evidence_ref=assessment.evidence_reference,
        )
        for assessment, segment in pairs
        if assessment.suspected
    )
