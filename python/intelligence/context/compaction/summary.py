"""Bounded conversation projection: what a summary may carry, and the budget it is packed under
(INT-008, DOMAIN.md §5.7, DOSSIER.md §8).

A long conversation is bounded by summarising its older part into a compaction epoch and projecting the
summary plus the recent turns. Two rules make that safe, and both are structural here rather than
conventional:

* **A summary carries narrative, never protocol truth.** The sections a summary may hold are a closed
  vocabulary of things a reader can be told — what happened, what was decided, what is still open, which
  evidence to consult. Tool calls, waits, pending model calls, browser/terminal holders, the next safe
  action and the effect ledger are *not* in it and cannot be put there, because replaying a run reads the
  event log and the protocol state, never a summary. Keeping them out is what makes "exact protocol
  replay does not depend on summary text" true of the plane rather than of the writer's discipline.
* **A projection never presents abandoned history.** The same staleness rule the Rust half enforces when
  an epoch is installed applies here when one is *used*: a summary whose range ends beyond the position
  the caller holds is dropped, with the reason recorded, instead of being shown to a model as if the run
  had got that far. A summary that cannot fit the budget is dropped whole — a summary's text is never
  truncated to fit, because half a summary is a fabrication.

The token accounting is INT-005's [`TokenLedger`][intelligence.context.projection.TokenLedger], so a
projection reports what it spent and what it left out the same way the rest of the context plane does.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum

from intelligence.context.projection import ContextError, ContextPolicy, Segment, TokenLedger

#: Refusal rules, named so a caller can tell which one fired.
RULE_SECTION_KIND = "summary.section_kind"
RULE_SECTION_SHAPE = "summary.section_shape"
RULE_SUMMARY_SHAPE = "summary.shape"
RULE_SUMMARY_RANGE = "summary.range"
RULE_PROJECTION = "projection.shape"


class SummarySectionKind(StrEnum):
    """The closed vocabulary of things a summary may carry.

    Everything here is something a reader can be *told*. Nothing here is state a run must obey, which is
    why this enum is short: the value of the list is the names it does not contain.
    """

    NARRATIVE = "narrative"
    DECISION = "decision"
    OPEN_QUESTION = "open_question"
    EVIDENCE_REF = "evidence_ref"


#: Section names a summary must never carry, listed so a refusal can say *why* rather than only "unknown".
#: These are the protocol-truth surfaces: a run's behaviour comes from the event log and the protocol
#: state (CORE-006), so summarising any of them would create a second, lossy copy of what a run obeys.
FORBIDDEN_SECTIONS: tuple[str, ...] = (
    "tool_call",
    "tool_calls",
    "pending_tool_call",
    "pending_model_call",
    "wait",
    "waits",
    "protocol_state",
    "next_action",
    "browser_control",
    "terminal_session",
    "effect_ledger",
    "effect_records",
    "checkpoint",
    "checkpoints",
    "generation",
)


@dataclass(frozen=True, slots=True)
class SummarySection:
    """One section of a summary: what kind of statement it is, its text, and what it costs."""

    kind: SummarySectionKind
    text: str
    tokens: int

    def __post_init__(self) -> None:
        if not self.text.strip():
            raise ContextError("VALIDATION_SCHEMA", RULE_SECTION_SHAPE, "a summary section needs text")
        if not isinstance(self.tokens, int) or isinstance(self.tokens, bool) or self.tokens < 1:
            raise ContextError(
                "VALIDATION_SCHEMA", RULE_SECTION_SHAPE, "a summary section needs a positive token count"
            )

    @classmethod
    def build(cls, *, kind: object, text: str, tokens: int) -> SummarySection:
        """Build a section, refusing protocol truth and unknown kinds.

        `kind` is deliberately typed `object`: a caller passing a raw string — from a model's JSON, say —
        is refused here with a reason naming what is wrong, rather than silently becoming an unvalidated
        section. Naming a forbidden section is refused with *why* it is forbidden, because the reason is
        the part a writer needs: a run obeys the event log and the protocol state, never a summary.
        """
        if not isinstance(kind, SummarySectionKind):
            name = str(kind).strip().lower()
            if name in FORBIDDEN_SECTIONS:
                raise ContextError(
                    "VALIDATION_SCHEMA",
                    RULE_SECTION_KIND,
                    f"{name!r} is protocol truth, not narrative: a summary may not carry it, because "
                    "exact protocol replay must not depend on summary text",
                )
            raise ContextError(
                "VALIDATION_SCHEMA",
                RULE_SECTION_KIND,
                f"{name!r} is not one of {[member.value for member in SummarySectionKind]}",
            )
        return cls(kind=kind, text=text, tokens=tokens)


@dataclass(frozen=True, slots=True)
class CompactionSummary:
    """A summary of a range of a thread's history, with the provenance that makes it citable."""

    summary_id: str
    thread_id: str
    source_from_sequence: int
    source_to_sequence: int
    sections: tuple[SummarySection, ...]
    model_route_id: str = ""

    def __post_init__(self) -> None:
        if not self.summary_id.strip() or not self.thread_id.strip():
            raise ContextError(
                "VALIDATION_SCHEMA", RULE_SUMMARY_SHAPE, "a summary needs its identity and its thread"
            )
        if self.source_to_sequence < self.source_from_sequence:
            raise ContextError(
                "VALIDATION_SCHEMA",
                RULE_SUMMARY_RANGE,
                f"summary range {self.source_from_sequence}..{self.source_to_sequence} is not ordered",
            )
        if not self.sections:
            raise ContextError(
                "VALIDATION_SCHEMA", RULE_SUMMARY_SHAPE, "a summary with no sections says nothing"
            )

    @property
    def tokens(self) -> int:
        """What the summary costs in the projection."""
        return sum(section.tokens for section in self.sections)

    def covers_position(self, position: int) -> bool:
        """Whether the caller's committed position still contains the range this summary covers.

        A revert moves the position back, so a summary ending beyond it describes history the run's
        lineage no longer contains.
        """
        return self.source_to_sequence <= position

    def narrative(self) -> str:
        """The summary as it is shown to a model: one labelled line per section, in order."""
        return "\n".join(f"[{section.kind.value}] {section.text}" for section in self.sections)


@dataclass(frozen=True, slots=True)
class BoundedProjection:
    """The bounded conversation a model sees, and the record of how it was chosen.

    Its fields are the summary, the segments and the ledger — there is deliberately no place for tool,
    wait or protocol state, so a projection cannot carry what a summary is forbidden to hold.
    """

    summary: CompactionSummary | None
    segments: tuple[Segment, ...]
    ledger: TokenLedger
    dropped_summary_reason: str = ""

    @property
    def items(self) -> int:
        """How many pieces the projection carries."""
        return len(self.segments) + (1 if self.summary is not None else 0)


def project_conversation(
    *,
    summary: CompactionSummary | None,
    segments: tuple[Segment, ...],
    position: int,
    policy: ContextPolicy,
) -> BoundedProjection:
    """Assemble a bounded conversation projection under the caller's token budget.

    The summary is used only when it still describes the caller's lineage (`position` contains its range)
    and only when it fits the whole budget on its own; otherwise it is dropped with the reason recorded
    and the recent segments are packed instead. Segments are kept in the order given until the budget is
    spent, and everything left out is accounted for in the ledger.
    """
    if position < 0:
        raise ContextError("VALIDATION_BOUNDS", RULE_PROJECTION, "a projection needs a committed position")

    used = 0
    dropped_summary = ""
    chosen_summary: CompactionSummary | None = None
    if summary is not None:
        if not summary.covers_position(position):
            dropped_summary = (
                f"summary covers up to sequence {summary.source_to_sequence}, "
                f"the caller holds {position}: abandoned history is not presented"
            )
        elif summary.tokens > policy.token_budget:
            dropped_summary = (
                f"summary needs {summary.tokens} tokens, the budget is {policy.token_budget}: a summary "
                "is dropped whole rather than truncated"
            )
        else:
            chosen_summary = summary
            used = summary.tokens

    chosen: list[Segment] = []
    dropped_tokens = 0
    dropped = 0
    dropped_by_channel: dict[str, int] = {}
    for segment in segments:
        if used + segment.tokens > policy.token_budget:
            dropped_tokens += segment.tokens
            dropped += 1
            dropped_by_channel[segment.channel] = dropped_by_channel.get(segment.channel, 0) + 1
            continue
        chosen.append(segment)
        used += segment.tokens

    return BoundedProjection(
        summary=chosen_summary,
        segments=tuple(chosen),
        ledger=TokenLedger(
            budget=policy.token_budget,
            used=used,
            dropped_tokens=dropped_tokens,
            segments=len(chosen),
            dropped_segments=dropped,
            dropped_by_channel=tuple(sorted(dropped_by_channel.items())),
        ),
        dropped_summary_reason=dropped_summary,
    )
