"""Turn context: the ContextProjection one model call is rendered from (INT-005, DOMAIN.md §11.2).

The runtime owns the canonical content of a turn — the teammate's instructions, the thread's
messages, the results of the tools it ran — and the snapshot it read them at. It hands them to the
Context plane as segment requests; the plane decides what the model sees and how:

* **Trust comes from the source.** Each segment is labelled from its source kind (and, for a thread
  turn, its role) per DOMAIN.md §12. A `trust_hint` from the caller can only lower the label, never
  raise it, so a caller bug cannot promote tool output to an instruction.
* **The budget is packed deterministically.** Required segments must fit or the build is refused.
  The rest is packed newest-first as one contiguous window: an assistant turn and the tool results
  that answer it are one unit, kept or dropped together, and a conversation never starts with an
  assistant or tool turn. Everything dropped is recorded as degradation, never hidden.
* **Rendering is the plane's.** Messages come out as the gateway's `RenderedMessage`: system and
  operator instructions first (the stable, cacheable prefix), then the conversation in `seq`
  order. Untrusted content is rendered inside the §12 typed boundary with the data-only
  instruction; the content contract is the gateway's own, so nothing the plane renders can be
  refused downstream for its shape.

The plane reads no canonical store and writes none: the result is a proposal the runtime validates
(`crates/server/src/runtime/context_bridge`) and records by its `ctx_` id.
"""

from __future__ import annotations

import hashlib
import json
import math
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from quansio.v1.intelligence import intelligence_pb2

from intelligence.context.projection import ContextError
from intelligence.model_gateway.content import PART_TEXT, parse_content
from intelligence.model_gateway.errors import GatewayError
from intelligence.model_gateway.ids import new_ulid
from intelligence.trust.labelling import BOUNDARY_CLOSE, BOUNDARY_OPEN, DATA_ONLY_INSTRUCTION, TrustLevel

SCHEMA_VERSION = "v1"
PROJECTION_PREFIX = "ctx_"

RULE_REQUEST_SHAPE = "turn.request_shape"
RULE_ROLE_FOR_SOURCE = "turn.role_for_source"
RULE_REQUIRED_BUDGET = "turn.required_budget"
RULE_SNAPSHOT_REQUIRED = "turn.snapshot_required"

#: DOMAIN.md §11.2 source kinds.
SOURCE_KINDS: tuple[str, ...] = (
    "system",
    "policy",
    "thread",
    "work",
    "tool_result",
    "knowledge",
    "memory",
    "artifact",
    "search",
)

_ROLES: Mapping[str, intelligence_pb2.RenderedMessage.Role] = {
    "system": intelligence_pb2.RenderedMessage.ROLE_SYSTEM,
    "operator": intelligence_pb2.RenderedMessage.ROLE_OPERATOR,
    "user": intelligence_pb2.RenderedMessage.ROLE_USER,
    "assistant": intelligence_pb2.RenderedMessage.ROLE_ASSISTANT,
    "tool": intelligence_pb2.RenderedMessage.ROLE_TOOL,
}

#: Which roles a source may be rendered in. Instructions travel in the instruction channels; data
#: never does — retrieved or produced content is rendered in a user turn, bounded when untrusted.
_ROLES_FOR_SOURCE: Mapping[str, frozenset[str]] = {
    "system": frozenset({"system", "operator"}),
    "policy": frozenset({"system", "operator"}),
    "thread": frozenset({"user", "assistant"}),
    "tool_result": frozenset({"tool"}),
    "work": frozenset({"user"}),
    "knowledge": frozenset({"user"}),
    "memory": frozenset({"user"}),
    "artifact": frozenset({"user"}),
    "search": frozenset({"user"}),
}

_REQUEST_KEYS = frozenset({"seq", "source", "role", "content", "trust_hint", "snapshot_ref", "required"})
_SOURCE_KEYS = frozenset({"kind", "ref"})
_INSTRUCTION_ROLES = frozenset({"system", "operator"})


def trust_for(kind: str, role: str) -> TrustLevel:
    """The DOMAIN.md §12 label a source carries; a thread turn is labelled by who wrote it."""
    if kind in ("system", "policy"):
        return TrustLevel.TRUSTED_SYSTEM
    if kind == "thread":
        return TrustLevel.TRUSTED_USER if role == "user" else TrustLevel.AGENT_GENERATED
    if kind == "work":
        return TrustLevel.TRUSTED_USER
    if kind == "knowledge":
        return TrustLevel.VERIFIED_KNOWLEDGE
    if kind == "memory":
        return TrustLevel.AGENT_GENERATED
    # tool_result, artifact, search, and anything unrecognised: data from outside.
    return TrustLevel.UNTRUSTED_EXTERNAL


@dataclass(frozen=True, slots=True)
class SegmentRequest:
    """One piece of canonical content the runtime offers for the turn."""

    seq: int
    kind: str
    ref: str
    role: str
    content_json: str
    trust: TrustLevel
    snapshot_ref: str
    required: bool
    tokens: int

    @property
    def is_instruction(self) -> bool:
        return self.role in _INSTRUCTION_ROLES


@dataclass(frozen=True, slots=True)
class TurnContext:
    """The built projection and the messages rendered from it."""

    projection: intelligence_pb2.ContextProjection

    @property
    def messages(self) -> Sequence[intelligence_pb2.RenderedMessage]:
        return self.projection.messages


def parse_segment_request(raw: str) -> SegmentRequest:
    """Parse one `segment_requests_json` entry, failing closed on any shape it does not know."""
    try:
        decoded: Any = json.loads(raw)
    except json.JSONDecodeError as error:
        raise _shape("a segment request is not valid JSON") from error
    if not isinstance(decoded, dict):
        raise _shape("a segment request must be a JSON object")
    unknown = sorted(set(decoded) - _REQUEST_KEYS)
    if unknown:
        raise _shape(f"a segment request has unknown key(s) {unknown}")
    seq = decoded.get("seq")
    if not isinstance(seq, int) or isinstance(seq, bool) or seq < 0:
        raise _shape("a segment request needs a non-negative integer `seq`")
    source = decoded.get("source")
    if not isinstance(source, dict) or set(source) - _SOURCE_KEYS:
        raise _shape("a segment request needs `source` {kind, ref}")
    kind, ref = source.get("kind"), source.get("ref")
    if kind not in SOURCE_KINDS or not isinstance(ref, str) or not ref.strip():
        raise _shape(f"segment {seq} names an unknown source kind or no source ref")
    role = decoded.get("role")
    if role not in _ROLES:
        raise _shape(f"segment {seq} names an unknown role {role!r}")
    if role not in _ROLES_FOR_SOURCE[kind]:
        raise ContextError(
            "VALIDATION_SCHEMA",
            RULE_ROLE_FOR_SOURCE,
            f"segment {seq}: a {kind} source cannot be rendered as a {role} message",
        )
    snapshot_ref = decoded.get("snapshot_ref")
    if not isinstance(snapshot_ref, str) or not snapshot_ref.strip():
        raise _shape(f"segment {seq} does not name the snapshot it was read at")
    required = decoded.get("required", False)
    if not isinstance(required, bool):
        raise _shape(f"segment {seq}: `required` must be a boolean")
    content = decoded.get("content")
    content_json = json.dumps(content, sort_keys=True, separators=(",", ":"))
    try:
        # The gateway's own content contract: the plane never renders what the gateway refuses.
        parse_content(content_json, is_tool_message=role == "tool", is_assistant_message=role == "assistant")
    except GatewayError as refused:
        raise ContextError(
            "VALIDATION_SCHEMA", RULE_REQUEST_SHAPE, f"segment {seq}: {refused.message}"
        ) from refused
    trust = trust_for(kind, role)
    hint = decoded.get("trust_hint")
    if hint is not None:
        try:
            hinted = TrustLevel(str(hint).lower())
        except ValueError as error:
            raise _shape(f"segment {seq} names an unknown trust hint {hint!r}") from error
        if hinted.rank > trust.rank:  # a hint only ever lowers trust
            trust = hinted
    if trust is TrustLevel.UNTRUSTED_EXTERNAL and role in _INSTRUCTION_ROLES:
        raise ContextError(
            "VALIDATION_SCHEMA",
            RULE_ROLE_FOR_SOURCE,
            f"segment {seq}: untrusted content cannot travel in an instruction channel",
        )
    return SegmentRequest(
        seq=seq,
        kind=kind,
        ref=ref,
        role=role,
        content_json=content_json,
        trust=trust,
        snapshot_ref=snapshot_ref,
        required=required,
        tokens=estimate_tokens(content_json),
    )


def estimate_tokens(content_json: str) -> int:
    """A deterministic, provider-independent estimate: four characters a token, at least one."""
    return max(1, math.ceil(len(content_json) / 4))


def build_turn_context(
    *,
    run_id: str,
    turn_id: str,
    snapshot_id: str,
    token_budget: int,
    policy_snapshot_digest: str,
    segment_requests: Sequence[str],
    now: datetime | None = None,
) -> TurnContext:
    """Label, pack and render one turn's context. Raises [`ContextError`]."""
    if not snapshot_id.strip():
        raise ContextError(
            "VALIDATION_SCHEMA", RULE_SNAPSHOT_REQUIRED, "a turn context must name the snapshot it reads"
        )
    if token_budget < 1:
        raise ContextError(
            "VALIDATION_BOUNDS", RULE_REQUIRED_BUDGET, "a turn context needs a positive budget"
        )
    segments = sorted((parse_segment_request(raw) for raw in segment_requests), key=lambda s: s.seq)
    seqs = [segment.seq for segment in segments]
    if len(set(seqs)) != len(seqs):
        raise _shape("segment requests repeat a `seq`")
    _check_tool_rounds([segment for segment in segments if not segment.is_instruction])

    kept, dropped = _pack(segments, token_budget)
    instructions = [segment for segment in kept if segment.is_instruction]
    conversation = [segment for segment in kept if not segment.is_instruction]
    ordered = [s for s in instructions if s.role == "system"] + [
        s for s in instructions if s.role == "operator"
    ]
    ordered += conversation

    projection = intelligence_pb2.ContextProjection(
        schema_version=SCHEMA_VERSION,
        run_id=run_id,
        turn_id=turn_id,
        policy_snapshot_digest=policy_snapshot_digest,
        snapshot_id=snapshot_id,
        created_at=(now or datetime.now(UTC)).strftime("%Y-%m-%dT%H:%M:%S.%fZ"),
    )
    for segment in ordered:
        projection.segments.append(
            intelligence_pb2.ContextSegment(
                schema_version=SCHEMA_VERSION,
                seq=segment.seq,
                source_kind=segment.kind,
                source_ref=segment.ref,
                trust_level=segment.trust.value,
                token_estimate=segment.tokens,
                snapshot_ref=segment.snapshot_ref,
            )
        )
        projection.messages.append(_render(segment))
    by_source: dict[str, int] = {}
    for segment in kept:
        by_source[segment.kind] = by_source.get(segment.kind, 0) + segment.tokens
    projection.token_ledger.CopyFrom(
        intelligence_pb2.TokenLedger(
            schema_version=SCHEMA_VERSION,
            budget=token_budget,
            used=sum(segment.tokens for segment in kept),
            by_source_json=json.dumps(by_source, sort_keys=True),
        )
    )
    for reason, group in dropped:
        by_kind: dict[str, int] = {}
        for segment in group:
            by_kind[segment.kind] = by_kind.get(segment.kind, 0) + 1
        for kind, count in sorted(by_kind.items()):
            projection.degradation_json.append(
                json.dumps({"source_kind": kind, "dropped": count, "reason": reason}, sort_keys=True)
            )
    digest = hashlib.sha256(projection.SerializeToString(deterministic=True)).hexdigest()
    projection.id = f"{PROJECTION_PREFIX}{new_ulid(seed=digest)}"
    return TurnContext(projection=projection)


def _pack(
    segments: Sequence[SegmentRequest], budget: int
) -> tuple[list[SegmentRequest], list[tuple[str, list[SegmentRequest]]]]:
    required = [segment for segment in segments if segment.required]
    used = sum(segment.tokens for segment in required)
    if used > budget:
        raise ContextError(
            "VALIDATION_BOUNDS",
            RULE_REQUIRED_BUDGET,
            f"required context needs {used} tokens; the budget is {budget}",
        )
    optional_instructions = [s for s in segments if not s.required and s.is_instruction]
    kept_ids = {segment.seq for segment in required}
    dropped_budget: list[SegmentRequest] = []
    for segment in optional_instructions:
        if used + segment.tokens <= budget:
            used += segment.tokens
            kept_ids.add(segment.seq)
        else:
            dropped_budget.append(segment)

    # The conversation, newest first, as one contiguous window of whole units.
    units = _units([segment for segment in segments if not segment.is_instruction])
    window_closed = False
    for unit in reversed(units):
        mandatory = any(segment.required for segment in unit)
        cost = sum(segment.tokens for segment in unit if segment.seq not in kept_ids)
        if not window_closed and (mandatory or used + cost <= budget):
            used += cost
            kept_ids.update(segment.seq for segment in unit)
        else:
            window_closed = True
            if mandatory:  # a required segment cannot be split from its unit
                raise ContextError(
                    "VALIDATION_BOUNDS",
                    RULE_REQUIRED_BUDGET,
                    "a required segment's turn does not fit the budget with the newer turns it follows",
                )
            dropped_budget.extend(unit)

    # A conversation opens with a user turn: an assistant or tool turn with nothing before it
    # has lost the question it answered.
    dropped_orphans: list[SegmentRequest] = []
    for unit in units:
        if not any(segment.seq in kept_ids for segment in unit):
            continue
        if unit[0].role == "user":
            break
        if any(segment.required for segment in unit):
            break
        kept_ids.difference_update(segment.seq for segment in unit)
        dropped_orphans.extend(unit)

    kept = [segment for segment in segments if segment.seq in kept_ids]
    dropped: list[tuple[str, list[SegmentRequest]]] = []
    if dropped_budget:
        dropped.append(("budget", dropped_budget))
    if dropped_orphans:
        dropped.append(("no_preceding_user_turn", dropped_orphans))
    return kept, dropped


def _check_tool_rounds(conversation: Sequence[SegmentRequest]) -> None:
    """Every tool result answers a call made by the assistant turn it follows."""
    for unit in _units(conversation):
        results = [segment for segment in unit if segment.role == "tool"]
        if not results:
            continue
        head = unit[0]
        called = (
            {
                call.id
                for call in parse_content(
                    head.content_json, is_tool_message=False, is_assistant_message=True
                ).tool_calls
            }
            if head.role == "assistant"
            else set()
        )
        for result in results:
            call_id = parse_content(result.content_json, is_tool_message=True).tool_call_id
            if call_id not in called:
                raise ContextError(
                    "VALIDATION_SCHEMA",
                    RULE_REQUEST_SHAPE,
                    f"segment {result.seq}: tool result {call_id!r} does not answer a call made by the "
                    "assistant turn before it",
                )


def _units(conversation: Sequence[SegmentRequest]) -> list[list[SegmentRequest]]:
    """Group an assistant turn with the tool results that follow it; every other turn stands alone."""
    units: list[list[SegmentRequest]] = []
    for segment in conversation:
        if segment.role == "tool" and units and units[-1][0].role == "assistant":
            units[-1].append(segment)
        else:
            units.append([segment])
    return units


def _render(segment: SegmentRequest) -> intelligence_pb2.RenderedMessage:
    content: Any = json.loads(segment.content_json)
    if segment.trust is TrustLevel.UNTRUSTED_EXTERNAL:
        content = _bounded(content, segment)
    return intelligence_pb2.RenderedMessage(
        schema_version=SCHEMA_VERSION,
        role=_ROLES[segment.role],
        trust_level=segment.trust.value,
        content_json=json.dumps(content, sort_keys=True, separators=(",", ":")),
        cache_hint="stable" if segment.is_instruction else "",
    )


def _bounded(content: Any, segment: SegmentRequest) -> Any:
    """Wrap every text part of untrusted content in the typed boundary (DOMAIN.md §12)."""

    def wrap(text: str) -> str:
        return (
            f"{BOUNDARY_OPEN} source={segment.kind}:{segment.ref} id=seg_{segment.seq}\n"
            f"{DATA_ONLY_INSTRUCTION}\n---\n{text}\n{BOUNDARY_CLOSE}"
        )

    if isinstance(content, str):
        return {"parts": [{"kind": PART_TEXT, "text": wrap(content)}]}
    parts = [
        {**part, "text": wrap(str(part.get("text", "")))} if part.get("kind") == PART_TEXT else part
        for part in content.get("parts", [])
    ]
    return {**content, "parts": parts}


def _shape(detail: str) -> ContextError:
    return ContextError("VALIDATION_SCHEMA", RULE_REQUEST_SHAPE, detail)
