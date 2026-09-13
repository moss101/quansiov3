"""The model gateway's embedding request class, as a route the derived index can embed through.

`config/models.yaml` names the embedding route and `config/derived` pins the width the index
stores, so this module is the join between them: it turns the gateway's fulfilled embedding
call into an [`EmbeddingProvider`], one instance per catalog route, and refuses a route whose
declared width is not the index's — before any call, which is what makes failover safe to
compose.

Routing, credentials, DLP and the wire stay in the gateway (INT-002/INT-003). Nothing here
talks to a provider, holds a credential or decides a route: it asks the gateway for vectors
and reports what the gateway said when it could not.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass

from intelligence.embeddings.provider import (
    RULE_GATEWAY_FAILED,
    EmbeddingError,
    EmbeddingProvider,
    FailoverEmbedder,
)
from intelligence.model_gateway import GatewayError, ModelGateway

#: Wall-clock bound for one embedding call when the caller names none.
DEFAULT_EMBEDDING_TIMEOUT_MS = 60_000


@dataclass(slots=True)
class GatewayEmbeddingProvider:
    """One catalog embedding route, fulfilled through the gateway.

    `dimensions` is the catalog's declared width for this route, not a guess: it is checked
    against the index pin when the failover list is built and against every response the
    gateway returns, so a route can never contribute a vector of another space.
    """

    gateway: ModelGateway
    model_id: str
    dimensions: int
    dlp_profile: str = ""
    timeout_ms: int = DEFAULT_EMBEDDING_TIMEOUT_MS

    @property
    def route_id(self) -> str:
        """Catalog model id this provider serves."""
        return self.model_id

    def embed(self, texts: Sequence[str], *, deadline_ms: int) -> tuple[tuple[float, ...], ...]:
        """Embed ``texts`` through the gateway, or refuse with the gateway's own code."""
        try:
            outcome = self.gateway.embed(
                texts,
                model_hint=self.model_id,
                dlp_profile=self.dlp_profile,
                timeout_ms=self.timeout_ms,
                # The index passes 0 for "no caller deadline": a deadline already in the past
                # would refuse every call the gateway is perfectly able to make.
                deadline_ms=deadline_ms if deadline_ms > 0 else None,
            )
        except GatewayError as failure:
            raise EmbeddingError(str(failure.code), RULE_GATEWAY_FAILED, failure.message) from failure
        if outcome.dimensions != self.dimensions:
            raise EmbeddingError(
                "VALIDATION_SCHEMA",
                RULE_GATEWAY_FAILED,
                f"route {self.model_id} served {outcome.dimensions}-wide vectors, "
                f"the catalog declares {self.dimensions}",
            )
        return outcome.vectors


def gateway_embedding_routes(gateway: ModelGateway) -> tuple[str, ...]:
    """The embedding routes the gateway resolved, in the order it would try them.

    The gateway owns routing, so the order is asked for rather than re-derived here: this only
    keeps a route the gateway would never select out of the failover list.
    """
    return gateway.embedding_routes()


def gateway_embedder(
    gateway: ModelGateway,
    *,
    dimensions: int,
    dlp_profile: str = "",
    timeout_ms: int = DEFAULT_EMBEDDING_TIMEOUT_MS,
) -> FailoverEmbedder:
    """Compose the index's embedder: every resolved embedding route, bounded by the index width.

    Raises [`EmbeddingError`] when no embedding route resolves, or when a resolved route's
    declared width is not ``dimensions`` — both before a single call is made.
    """
    catalog = gateway.catalog
    providers: list[EmbeddingProvider] = []
    for model_id in gateway_embedding_routes(gateway):
        spec = catalog.models[model_id]
        if spec.embedding_dimensions is None:  # pragma: no cover - the catalog refuses this
            continue
        providers.append(
            GatewayEmbeddingProvider(
                gateway=gateway,
                model_id=model_id,
                dimensions=spec.embedding_dimensions,
                dlp_profile=dlp_profile,
                timeout_ms=timeout_ms,
            )
        )
    return FailoverEmbedder(providers=tuple(providers), dimensions=dimensions)
