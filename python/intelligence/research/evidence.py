"""EvidenceBundle with deterministic citation checks (CAP-002, DOMAIN.md §10).

A bundle binds answer claims to digest-addressed evidence descriptors. Two
checks are deterministic and run before any verified completion:

* **existence** — every citation must resolve to a descriptor in the bundle;
* **coverage** — every claim must cite at least one descriptor.

A bundle round-trips through its content digest: the same claims and
descriptors always rebuild the same bundle identity, so the evidence survives
a provider/model change — the bundle is the authority, not the run that made it.
A verified completion requires `verify` to pass; missing or invalid evidence
prevents it.
"""

from __future__ import annotations

import hashlib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass

from .workflow import EvidenceDescriptor


class EvidenceError(ValueError):
    """A refused bundle or a failed verification, naming the rule."""

    def __init__(self, code: str, detail: str) -> None:
        super().__init__(f"{code}: {detail}")
        self.code = code
        self.detail = detail


@dataclass(frozen=True, slots=True)
class CitedClaim:
    """One claim and the descriptor ids that must support it."""

    claim_id: str
    text: str
    citation_ids: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class EvidenceBundle:
    """Claims + descriptors, content-addressed by `bundle_digest`."""

    bundle_id: str
    claims: tuple[CitedClaim, ...]
    descriptors: tuple[EvidenceDescriptor, ...]
    bundle_digest: str
    model_route: str = ""
    provider: str = ""

    @classmethod
    def build(
        cls,
        bundle_id: str,
        claims: Sequence[CitedClaim],
        descriptors: Sequence[EvidenceDescriptor],
        *,
        model_route: str = "",
        provider: str = "",
    ) -> EvidenceBundle:
        if not bundle_id.startswith("evb_"):
            raise EvidenceError("VALIDATION_SCHEMA", "bundle id must start with evb_")
        if not claims:
            raise EvidenceError("VALIDATION_SCHEMA", "a bundle with no claims verifies nothing")
        if not descriptors:
            raise EvidenceError("VALIDATION_SCHEMA", "a bundle with no descriptors supports nothing")
        ids = [item.claim_id for item in claims]
        if len(set(ids)) != len(ids):
            raise EvidenceError("VALIDATION_SCHEMA", "claim ids must be unique")
        by_id = {item.source_id: item for item in descriptors}
        for claim in claims:
            if not claim.citation_ids:
                raise EvidenceError(
                    "VALIDATION_SCHEMA",
                    f"claim {claim.claim_id!r} cites nothing; uncited claims verify nothing",
                )
            for citation in claim.citation_ids:
                if citation not in by_id:
                    raise EvidenceError(
                        "VALIDATION_SCHEMA",
                        f"claim {claim.claim_id!r} cites {citation!r}, which is not in the bundle",
                    )
        material = "\n".join(
            [bundle_id]
            + [f"{c.claim_id}|{c.text}|{','.join(c.citation_ids)}" for c in claims]
            + [f"{d.source_id}|{d.uri}|{d.content_digest}" for d in descriptors]
        )
        digest = hashlib.sha256(material.encode("utf-8")).hexdigest()
        return cls(
            bundle_id=bundle_id,
            claims=tuple(claims),
            descriptors=tuple(descriptors),
            bundle_digest=digest,
            model_route=model_route,
            provider=provider,
        )

    def as_mapping(self) -> Mapping[str, object]:
        return {
            "bundle_id": self.bundle_id,
            "bundle_digest": self.bundle_digest,
            "model_route": self.model_route,
            "provider": self.provider,
            "claims": [
                {"claim_id": c.claim_id, "text": c.text, "citations": list(c.citation_ids)}
                for c in self.claims
            ],
            "descriptors": [d.as_mapping() for d in self.descriptors],
        }


def verify(bundle: EvidenceBundle, claims: Sequence[CitedClaim]) -> tuple[bool, list[str]]:
    """Deterministic existence + coverage check for `claims` against `bundle`.

    Returns `(all_hold, failures)`; a failed check prevents verified completion.
    """
    failures: list[str] = []
    known = {item.source_id: item for item in bundle.descriptors}
    bundle_claims = {item.claim_id: item for item in bundle.claims}
    for claim in claims:
        held = bundle_claims.get(claim.claim_id)
        if held is None:
            failures.append(f"{claim.claim_id}: not in the bundle")
            continue
        if not claim.citation_ids:
            failures.append(f"{claim.claim_id}: no citations")
            continue
        for citation in claim.citation_ids:
            descriptor = known.get(citation)
            if descriptor is None:
                failures.append(f"{claim.claim_id}: broken citation {citation}")
                continue
            expected = hashlib.sha256(descriptor.text.encode("utf-8")).hexdigest()
            if expected != descriptor.content_digest:
                failures.append(f"{claim.claim_id}: {citation} digest drift")
    return (not failures, failures)
