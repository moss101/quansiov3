"""ContextProjection: the bounded, model-visible context (DOMAIN.md §11.2, INT-005).

A projection is built from canonical sources under an explicit policy, and it records what it was
built from: the program, the source snapshot, the policy and a token ledger. Two rules make it
trustworthy rather than convenient:

* **Every segment carries a `trust_level`** (DOMAIN.md §12). A segment without one is refused when
  it is built, so trust is never inferred from position or assumed by a reader.
* **A bundle knows its snapshot.** A projection built from a snapshot that is no longer current is
  refused rather than served, so a model never reasons over context that has moved underneath it.

Packing is deterministic: rank, deduplicate, then fill the token budget in order, recording what
was dropped so degradation is reported instead of hidden.
"""

from __future__ import annotations

import hashlib
from collections.abc import Iterable
from dataclasses import dataclass
from enum import StrEnum


class ContextError(ValueError):
    """A refused projection, naming the rule that refused it."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


RULE_TRUST_REQUIRED = "segment.trust_required"
RULE_SEGMENT_SHAPE = "segment.shape"
RULE_BUDGET = "projection.budget"
RULE_SNAPSHOT = "projection.snapshot"
RULE_POLICY = "projection.policy"

#: The DOMAIN.md §12 trust ladder, most trusted first, as the generated contract spells it.
TRUST_LEVELS: tuple[str, ...] = (
    "trusted_system",
    "trusted_user",
    "verified_knowledge",
    "agent_generated",
    "untrusted_external",
)

#: The levels whose content is stable enough to belong to a cacheable prefix.
STABLE_TRUST_LEVELS: tuple[str, ...] = ("trusted_system", "trusted_user", "verified_knowledge")


class TrustLevel(StrEnum):
    """The trust level a segment's content carries (DOMAIN.md §12)."""

    TRUSTED_SYSTEM = "trusted_system"
    TRUSTED_USER = "trusted_user"
    VERIFIED_KNOWLEDGE = "verified_knowledge"
    AGENT_GENERATED = "agent_generated"
    UNTRUSTED_EXTERNAL = "untrusted_external"

    @property
    def rank(self) -> int:
        """Position in the ladder, most trusted first."""
        return TRUST_LEVELS.index(self.value)

    @property
    def contract_name(self) -> str:
        """The generated contract spelling, for the parity test."""
        return f"TRUST_LEVEL_{self.value.upper()}"

    @property
    def is_stable(self) -> bool:
        """Whether content at this level may enter the cacheable stable prefix."""
        return self.value in STABLE_TRUST_LEVELS

    @property
    def is_data_only(self) -> bool:
        """Whether the content is data that can never carry intent."""
        return self is TrustLevel.UNTRUSTED_EXTERNAL


@dataclass(frozen=True, slots=True)
class Segment:
    """One piece of context, with the provenance and trust it was read at."""

    segment_id: str
    text: str
    tokens: int
    trust_level: TrustLevel
    source: str
    snapshot: str
    channel: str
    score: float = 0.0
    evidence_id: str | None = None

    @classmethod
    def build(
        cls,
        *,
        segment_id: str,
        text: str,
        tokens: int,
        trust_level: object,
        source: str,
        snapshot: str,
        channel: str,
        score: float = 0.0,
        evidence_id: str | None = None,
    ) -> Segment:
        """Build a segment, refusing one that carries no trust label.

        `trust_level` is deliberately typed `object` so a caller passing `None`, a raw string or an
        enum from somewhere else is refused here rather than becoming an unlabelled segment.

        Raises [`ContextError`] (`VALIDATION_SCHEMA`) for a missing trust level or a malformed
        segment.
        """
        if not isinstance(trust_level, TrustLevel):
            raise ContextError(
                "VALIDATION_SCHEMA",
                RULE_TRUST_REQUIRED,
                f"segment {segment_id!r} carries no trust level (got {trust_level!r}); "
                "every segment needs one",
            )
        if not segment_id.strip() or not source.strip() or not snapshot.strip():
            raise ContextError(
                "VALIDATION_SCHEMA",
                RULE_SEGMENT_SHAPE,
                "a segment needs an id, a source and the snapshot it was read at",
            )
        if not channel.strip():
            raise ContextError("VALIDATION_SCHEMA", RULE_SEGMENT_SHAPE, "a segment needs a channel")
        if not isinstance(tokens, int) or isinstance(tokens, bool) or tokens < 0:
            raise ContextError(
                "VALIDATION_SCHEMA", RULE_SEGMENT_SHAPE, "segment tokens must be a non-negative integer"
            )
        return cls(
            segment_id=segment_id,
            text=text,
            tokens=tokens,
            trust_level=trust_level,
            source=source,
            snapshot=snapshot,
            channel=channel,
            score=score,
            evidence_id=evidence_id,
        )

    @property
    def content_digest(self) -> str:
        """A stable digest of the segment's text, used for deduplication."""
        return hashlib.sha256(self.text.encode("utf-8")).hexdigest()


@dataclass(frozen=True, slots=True)
class ContextPolicy:
    """How a projection is built: budget, trust floor and deduplication."""

    token_budget: int
    min_trust: TrustLevel = TrustLevel.UNTRUSTED_EXTERNAL
    dedupe: bool = True

    def __post_init__(self) -> None:
        if not isinstance(self.token_budget, int) or self.token_budget < 1:
            raise ContextError("VALIDATION_BOUNDS", RULE_POLICY, "a projection needs a positive token budget")


@dataclass(frozen=True, slots=True)
class TokenLedger:
    """What the projection spent, and what it left out."""

    budget: int
    used: int
    dropped_tokens: int
    segments: int
    dropped_segments: int
    dropped_by_channel: tuple[tuple[str, int], ...] = ()

    @property
    def remaining(self) -> int:
        return max(0, self.budget - self.used)


@dataclass(frozen=True, slots=True)
class ContextBundle:
    """A packed projection: what the model sees, and the record of how it was chosen."""

    program_key: str
    snapshot_id: str
    policy: ContextPolicy
    segments: tuple[Segment, ...]
    ledger: TokenLedger
    degraded: bool
    degradation: tuple[str, ...] = ()

    def render_stable_prefix(self) -> str:
        """The cacheable prefix: stable-trust segments, in a deterministic order.

        Only `trusted_system`, `trusted_user` and `verified_knowledge` content belongs here, and
        the order is a pure function of (source, segment id), so the prefix is byte-identical for
        the same set of segments however the volatile tail changes — which is what makes prefix
        caching safe.
        """
        stable = [segment for segment in self.segments if segment.trust_level.is_stable]
        stable.sort(key=lambda segment: (segment.source, segment.segment_id))
        return "\n\n".join(
            f"[{segment.trust_level.value}:{segment.source}] {segment.text}" for segment in stable
        )

    def render(self) -> str:
        """The whole projection: the stable prefix first, then the volatile tail in rank order."""
        tail = [segment for segment in self.segments if not segment.trust_level.is_stable]
        rendered = [self.render_stable_prefix()] if self.segments else []
        rendered.extend(f"[{segment.trust_level.value}:{segment.source}] {segment.text}" for segment in tail)
        return "\n\n".join(part for part in rendered if part)

    def is_stale(self, current_snapshot: str) -> bool:
        """Whether the canonical sources have moved since this bundle was built."""
        return self.snapshot_id != current_snapshot


def build_bundle(
    *,
    program_key: str,
    snapshot_id: str,
    policy: ContextPolicy,
    segments: Iterable[Segment],
    current_snapshot: str | None = None,
) -> ContextBundle:
    """Rank, deduplicate and pack segments into a bounded projection.

    Refuses a stale projection (`CONFLICT_STATE`) when `current_snapshot` no longer matches, which
    is the rule that keeps a model from reasoning over context that has moved.

    Raises [`ContextError`].
    """
    if current_snapshot is not None and snapshot_id != current_snapshot:
        raise ContextError(
            "CONFLICT_STATE",
            RULE_SNAPSHOT,
            f"the projection was built from snapshot {snapshot_id!r} but the sources are at "
            f"{current_snapshot!r}",
        )
    candidates = list(segments)
    for segment in candidates:
        if not isinstance(segment, Segment):
            raise ContextError("VALIDATION_SCHEMA", RULE_SEGMENT_SHAPE, "not a segment")
        if segment.trust_level.rank > policy.min_trust.rank:
            raise ContextError(
                "VALIDATION_SCHEMA",
                RULE_TRUST_REQUIRED,
                f"segment {segment.segment_id!r} is {segment.trust_level.value}, below the policy "
                f"floor {policy.min_trust.value}",
            )

    # Deterministic ranking: score desc, then the more trusted level, then the segment id.
    ranked = sorted(
        candidates,
        key=lambda segment: (-segment.score, segment.trust_level.rank, segment.segment_id),
    )
    kept: list[Segment] = []
    dropped: list[Segment] = []
    seen: set[tuple[str, str]] = set()
    used = 0
    for segment in ranked:
        key = (segment.source, segment.content_digest)
        if policy.dedupe and key in seen:
            dropped.append(segment)
            continue
        if used + segment.tokens > policy.token_budget:
            dropped.append(segment)
            continue
        seen.add(key)
        used += segment.tokens
        kept.append(segment)

    by_channel: dict[str, int] = {}
    for segment in dropped:
        by_channel[segment.channel] = by_channel.get(segment.channel, 0) + 1
    ledger = TokenLedger(
        budget=policy.token_budget,
        used=used,
        dropped_tokens=sum(segment.tokens for segment in dropped),
        segments=len(kept),
        dropped_segments=len(dropped),
        dropped_by_channel=tuple(sorted(by_channel.items())),
    )
    degradation = tuple(
        f"{channel}: {count} segment(s) dropped" for channel, count in ledger.dropped_by_channel
    )
    return ContextBundle(
        program_key=program_key,
        snapshot_id=snapshot_id,
        policy=policy,
        segments=tuple(kept),
        ledger=ledger,
        degraded=bool(dropped),
        degradation=degradation,
    )
