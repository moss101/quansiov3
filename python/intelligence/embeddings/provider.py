"""Embedding providers and the failover order the index embeds through (INT-011).

The pipeline never names a model itself: it asks a provider for vectors, and the provider
list is ordered by the catalog's fallback order (``config/models.yaml``), so routing stays
where it belongs. Two rules make failover safe for a derived index rather than merely
convenient:

* **Dimensions are a hard constraint.** Every candidate must produce the same width as the
  index; a provider that would produce a different width is refused *before* any call, not
  discovered when a vector cannot be written. Failover moves between endpoints, never
  between embedding spaces.
* **A failure is recorded, not hidden.** Every attempt (route, outcome, reason) is kept, so a
  failover is observable and a total failure names what each candidate said.

The gateway (INT-002/INT-003) is the implementation that owns the wire, credentials and DLP;
this module owns only the order and the width guarantee.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Protocol

#: Refusal rules, named so a caller can tell which one fired.
RULE_NO_PROVIDERS = "embedding.no_providers"
RULE_DIMENSION_MISMATCH = "embedding.dimension_mismatch"
RULE_PROVIDER_FAILED = "embedding.provider_failed"
RULE_BAD_VECTOR = "embedding.bad_vector"
RULE_EMPTY_INPUT = "embedding.empty_input"
RULE_GATEWAY_FAILED = "embedding.gateway_failed"


class EmbeddingError(RuntimeError):
    """An embedding request that could not be fulfilled (never a partial vector set)."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


class EmbeddingProvider(Protocol):
    """One embedding route: what it is, how wide it is, and how to get vectors from it."""

    @property
    def route_id(self) -> str:
        """Catalog model id this provider serves (``config/models.yaml``)."""

    @property
    def dimensions(self) -> int:
        """Vector width this provider produces."""

    def embed(self, texts: Sequence[str], *, deadline_ms: int) -> tuple[tuple[float, ...], ...]:
        """Embed every text, returning one vector per input in input order."""


@dataclass(frozen=True, slots=True)
class Attempt:
    """What one candidate route did during one call."""

    route_id: str
    outcome: str
    detail: str


@dataclass(slots=True)
class FailoverEmbedder:
    """Embed through an ordered candidate list, refusing width changes and recording attempts.

    The list is the catalog's fallback order: the first candidate that answers wins, and a
    candidate that fails is skipped — but only when it produces the index's width at all, and
    never silently.
    """

    providers: tuple[EmbeddingProvider, ...]
    dimensions: int
    attempts: list[Attempt] = field(default_factory=list)

    def __post_init__(self) -> None:
        if not self.providers:
            raise EmbeddingError(
                "PROVIDER_UNAVAILABLE",
                RULE_NO_PROVIDERS,
                "no embedding route is configured; the catalog must declare an embedding model",
            )
        mismatched = [
            provider.route_id for provider in self.providers if provider.dimensions != self.dimensions
        ]
        if mismatched:
            raise EmbeddingError(
                "VALIDATION_SCHEMA",
                RULE_DIMENSION_MISMATCH,
                f"routes {mismatched} do not produce the index width {self.dimensions}",
            )

    @property
    def route_ids(self) -> tuple[str, ...]:
        """The candidate order, for reporting."""
        return tuple(provider.route_id for provider in self.providers)

    @property
    def route_id(self) -> str:
        """The primary route's id: the route a call starts with and reports as its own.

        A failover embedder *is* an [`EmbeddingProvider`], so it answers with the route it would
        try first; `served_by` says which route actually answered the last call.
        """
        return self.providers[0].route_id

    @property
    def served_by(self) -> str | None:
        """The route that answered the last call, or None when none has answered yet."""
        for attempt in reversed(self.attempts):
            if attempt.outcome == "answered":
                return attempt.route_id
        return None

    def embed(self, texts: Sequence[str], *, deadline_ms: int) -> tuple[tuple[float, ...], ...]:
        """Embed ``texts`` through the first route that answers.

        Raises [`EmbeddingError`] when the input is empty, a route returns a malformed or
        wrongly-sized set of vectors, or every route failed.
        """
        if not texts:
            raise EmbeddingError("VALIDATION_SCHEMA", RULE_EMPTY_INPUT, "no text to embed")
        self.attempts.clear()
        failures: list[str] = []
        for provider in self.providers:
            try:
                vectors = self._checked(provider, texts, deadline_ms)
            except EmbeddingError as error:
                self.attempts.append(Attempt(provider.route_id, "failed", error.detail))
                failures.append(f"{provider.route_id}: {error.detail}")
                continue
            self.attempts.append(Attempt(provider.route_id, "answered", f"{len(vectors)} vectors"))
            return vectors
        raise EmbeddingError(
            "PROVIDER_UNAVAILABLE",
            RULE_PROVIDER_FAILED,
            "every embedding route failed: " + "; ".join(failures),
        )

    def _checked(
        self,
        provider: EmbeddingProvider,
        texts: Sequence[str],
        deadline_ms: int,
    ) -> tuple[tuple[float, ...], ...]:
        try:
            vectors = tuple(tuple(vector) for vector in provider.embed(texts, deadline_ms=deadline_ms))
        except EmbeddingError:
            # A width refusal is ours, not the route's: it must not be retried elsewhere.
            raise
        except Exception as error:
            raise EmbeddingError("PROVIDER_UNAVAILABLE", RULE_PROVIDER_FAILED, str(error)) from error
        if len(vectors) != len(texts):
            raise EmbeddingError(
                "VALIDATION_SCHEMA",
                RULE_BAD_VECTOR,
                f"{provider.route_id} returned {len(vectors)} vectors for {len(texts)} texts",
            )
        for vector in vectors:
            if len(vector) != self.dimensions:
                raise EmbeddingError(
                    "VALIDATION_SCHEMA",
                    RULE_BAD_VECTOR,
                    f"{provider.route_id} returned a {len(vector)}-wide vector, expected {self.dimensions}",
                )
        return vectors
