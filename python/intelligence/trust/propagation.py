"""Propagating segment trust onto proposal elements (INT-012 build item 2).

A proposal element (a tool call, a message, a plan node) may reference several context segments. Its
`derived_from_trust` is the **most untrusted** label among the segments it actually references, which
is the property the runtime's policy escalation depends on: if any part of an element's causal chain
reached untrusted content, the element as a whole is untrusted-derived.

Two fail-closed rules make this safe to rely on:

* an element that references **nothing** is not treated as trusted — the caller must say what it
  derived from, and an unprovable chain is the runtime's `FAIL_CLOSED_TRUST`;
* a reference to a segment the labelling does not know is untrusted, because an unknown segment is
  an unknown origin.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass

from intelligence.trust.labelling import TrustLevel


class PropagationError(ValueError):
    """A refused propagation request."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(f"{code}: {detail} (rule {rule_id})")
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


RULE_UNKNOWN_SEGMENT = "propagation.unknown_segment"
RULE_NO_REFS = "propagation.no_references"


@dataclass(frozen=True, slots=True)
class Propagation:
    """What one element derived from."""

    element_id: str
    trust: TrustLevel
    referenced: tuple[str, ...]
    unknown: tuple[str, ...]

    @property
    def is_untrusted_derived(self) -> bool:
        """Whether the element's chain reached untrusted content."""
        return self.trust.is_data_only or bool(self.unknown)


def derive(
    *,
    element_id: str,
    refs: Iterable[str],
    segment_trust: Mapping[str, TrustLevel],
) -> Propagation:
    """Derive one element's trust from the segments it references.

    # Raises
    `PropagationError` (`VALIDATION_SCHEMA`) when the element references nothing, because an
    unprovable chain must be stated by the caller rather than assumed trusted here.
    """
    referenced = tuple(refs)
    if not referenced:
        raise PropagationError(
            "VALIDATION_SCHEMA",
            RULE_NO_REFS,
            f"element {element_id!r} references no segment, so its trust cannot be derived",
        )
    unknown = tuple(sorted(ref for ref in referenced if ref not in segment_trust))
    labels = [segment_trust[ref] for ref in referenced if ref in segment_trust]
    # An unknown segment is an unknown origin: the element is untrusted-derived regardless of how
    # trustworthy its known references are.
    weakest = TrustLevel.UNTRUSTED_EXTERNAL if unknown else max(labels, key=lambda level: level.rank)
    return Propagation(
        element_id=element_id,
        trust=weakest,
        referenced=tuple(sorted(referenced)),
        unknown=unknown,
    )


def derive_all(
    *,
    elements: Mapping[str, Iterable[str]],
    segment_trust: Mapping[str, TrustLevel],
) -> tuple[Propagation, ...]:
    """Derive every element, in the order the caller supplied them.

    # Raises
    `PropagationError` for an element with no references, so one unprovable element cannot be
    quietly skipped.
    """
    return tuple(
        derive(element_id=element_id, refs=refs, segment_trust=segment_trust)
        for element_id, refs in elements.items()
    )
