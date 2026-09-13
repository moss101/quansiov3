"""The one gateway every model fulfillment goes through (D-006, DOSSIER.md §7).

`ModelCallRequest` in, normalized `ModelEvent` stream out. The gateway owns four things no
adapter may: route resolution, credential materialization, cancellation/timeout enforcement,
and usage/terminal-outcome accounting. It executes no tools, commits no WorkGraph state and
mutates no external system — it proposes text and tool calls to the runtime.

Ordering contract for one call (DOMAIN.md §11.1):

    CALL_STARTED → DELTA*/TOOL_CALL*/THINKING_SUMMARY* → [ERROR] → USAGE → STOP → CALL_COMPLETED

Exactly one USAGE event and exactly one STOP event are emitted per call, and exactly one
`TerminalOutcome` is recorded — on success, refusal, provider error, timeout and cancellation
alike. Cancellation closes the provider socket immediately and records
`MODEL_STOP_REASON_CANCELLED`; usage recorded for a cancelled call is what the provider had
actually reported, never a partial guess. A provider that never answers is bounded by the
call's `timeout_ms` and surfaces `TOOL_TIMEOUT` with `retryable=True`.

Route selection here is the minimal deterministic INT-002 rule (`selection.py`); INT-003 owns
the full policy and replaces the injected `RouteSelector`.
"""

from __future__ import annotations

import contextlib
import json
import logging
import os
import threading
import time
from collections.abc import Callable, Generator, Iterator, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

from quansio.v1.intelligence import intelligence_pb2

from intelligence.model_gateway.adapters import (
    PreparedRequest,
    ProviderAdapter,
    ProviderCallContext,
    ProviderResult,
    adapter_for_kind,
    http_error_result,
    parse_messages,
)
from intelligence.model_gateway.adapters.base import (
    assert_no_disabled_features,
    assert_operator_channel,
)
from intelligence.model_gateway.catalog import ModelCatalog
from intelligence.model_gateway.credentials import CredentialResolver
from intelligence.model_gateway.dlp import DlpDecision, DlpGuard
from intelligence.model_gateway.embeddings import (
    EmbeddingCallContext,
    check_width,
    embedding_wire_for_kind,
    response_byte_budget,
    validate_embedding_inputs,
)
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.events import (
    EventFactory,
    TerminalOutcome,
    estimate_cost_minor_units,
)
from intelligence.model_gateway.ids import new_ulid
from intelligence.model_gateway.routing import PolicyRouteSelector, RequestClass
from intelligence.model_gateway.selection import CatalogPrimarySelector, RouteDecision, RouteSelector
from intelligence.model_gateway.tooling import MappingToolSchemaSource, ToolSchemaSource, resolve_tools
from intelligence.model_gateway.transport import (
    HttpRequest,
    HttpTransport,
    StdlibHttpTransport,
    StreamCancelledError,
    StreamResponse,
    TransportError,
    TransportTimeoutError,
)

LOGGER_NAME = "intelligence.model_gateway"

#: Default wall-clock bound for one embedding call when the caller names none.
DEFAULT_EMBEDDING_TIMEOUT_MS = 60_000


@dataclass(frozen=True, slots=True)
class EmbeddingOutcome:
    """One fulfilled embedding call: the vectors, in input order, and the route that produced them."""

    model_catalog_id: str
    wire_model_id: str
    provider: str
    route_id: str
    dimensions: int
    vectors: tuple[tuple[float, ...], ...]
    chosen_by: str


@dataclass(frozen=True, slots=True)
class PreparedCall:
    """Everything resolved before transmission: route, adapter, custody and the wire request."""

    route: RouteDecision
    adapter: ProviderAdapter
    prepared_request: PreparedRequest
    cancellation: CancellationToken
    budget_ms: int


class CancellationToken:
    """Cancellation signal shared between the runtime caller and a live fulfilment."""

    __slots__ = ("_event", "_token_id")

    def __init__(self, token_id: str = "") -> None:
        self._token_id = token_id
        self._event = threading.Event()

    @property
    def token_id(self) -> str:
        return self._token_id

    def cancel(self) -> None:
        self._event.set()

    def is_cancelled(self) -> bool:
        return self._event.is_set()


class CancellationRegistry:
    """Maps `ModelCallRequest.cancellation_token` values to live cancellation signals."""

    __slots__ = ("_lock", "_tokens")

    def __init__(self) -> None:
        self._tokens: dict[str, CancellationToken] = {}
        self._lock = threading.Lock()

    def for_token(self, token_id: str) -> CancellationToken:
        with self._lock:
            token = self._tokens.get(token_id)
            if token is None:
                token = CancellationToken(token_id)
                self._tokens[token_id] = token
            return token

    def cancel(self, token_id: str) -> bool:
        """Cancel the live call registered under `token_id`; False when nothing is in flight."""
        with self._lock:
            token = self._tokens.get(token_id)
        if token is None:
            return False
        token.cancel()
        return True

    def forget(self, token_id: str) -> None:
        if not token_id:
            return
        with self._lock:
            self._tokens.pop(token_id, None)


class ModelGateway:
    """Trusted model fulfillment: catalog-driven routing, adapters, custody and accounting.

    `FulfillModel` is a server-streaming RPC, so every call streams; `ModelCallRequest.stream`
    is advisory. The gateway proposes text and tool calls only — it never executes a tool,
    writes canonical state or mutates an external system.
    """

    def __init__(
        self,
        *,
        catalog: ModelCatalog,
        environ: Mapping[str, str] | None = None,
        credentials: CredentialResolver | None = None,
        tool_schemas: ToolSchemaSource | None = None,
        transport: HttpTransport | None = None,
        selector: RouteSelector | None = None,
        embedding_selector: RouteSelector | None = None,
        dlp: DlpGuard | None = None,
        clock: Callable[[], float] = time.time,
        logger: logging.Logger | None = None,
    ) -> None:
        self._catalog = catalog
        self._environ: Mapping[str, str] = os.environ if environ is None else environ
        self._credentials = credentials if credentials is not None else CredentialResolver(self._environ)
        self._tool_schemas = tool_schemas if tool_schemas is not None else MappingToolSchemaSource()
        self._transport = transport if transport is not None else StdlibHttpTransport()
        # INT-003 owns selection policy and provides `PolicyRouteSelector`; which selector a
        # deployment runs is the composition root's decision, so an uninjected gateway keeps the
        # INT-002 path and the seam stays injectable (see HANDOFF.md).
        self._selector = selector if selector is not None else CatalogPrimarySelector()
        # Embedding routes are resolved by class, never by `routing.primary`: the catalog's
        # primary model leads *chat*, and reusing that here would send an embedding call to a
        # model with no embeddings endpoint. INT-003's selector is class-driven, so the
        # embedding class gets its own instance of it.
        self._embedding_selector = (
            embedding_selector
            if embedding_selector is not None
            else PolicyRouteSelector(default_class=RequestClass.EMBEDDING)
        )
        self._dlp = dlp
        self._clock = clock
        self._logger = logger if logger is not None else logging.getLogger(LOGGER_NAME)
        self._cancellations = CancellationRegistry()
        self._dlp_decision: DlpDecision | None = None

    @classmethod
    def from_environment(
        cls,
        *,
        catalog_path: Path | None = None,
        environ: Mapping[str, str] | None = None,
        tool_schemas: ToolSchemaSource | None = None,
        logger: logging.Logger | None = None,
    ) -> ModelGateway:
        """Compose the production gateway from `config/models.yaml` and process environment."""
        return cls(
            catalog=ModelCatalog.load(catalog_path),
            environ=environ,
            tool_schemas=tool_schemas,
            transport=StdlibHttpTransport(),
            selector=CatalogPrimarySelector(),
            logger=logger,
        )

    @property
    def catalog(self) -> ModelCatalog:
        return self._catalog

    @property
    def cancellations(self) -> CancellationRegistry:
        return self._cancellations

    @property
    def dlp_decision(self) -> DlpDecision | None:
        """The data-policy decision of the last prepared call, when one was prepared."""
        return getattr(self, "_dlp_decision", None)

    def resolve_route(self, request: intelligence_pb2.ModelCallRequest) -> RouteDecision:
        """Deterministic route resolution; raises `ROUTE_UNAVAILABLE` when nothing resolves."""
        return self._selector.select(self._catalog, request)

    def cancel(self, cancellation_token: str) -> bool:
        """Cancel a live call by its request `cancellation_token`."""
        return self._cancellations.cancel(cancellation_token)

    def embedding_routes(self) -> tuple[str, ...]:
        """Catalog ids an embedding call may use, in the order routing would try them.

        The first entry is the route `embed` resolves. Only models that declare the
        `embeddings` capability *and* a width are offered, because a route the gateway cannot
        check a width against, or one with no embeddings endpoint, must not appear as a
        failover target the index could then prefer over a working route.
        """
        probe = intelligence_pb2.ModelCallRequest(
            schema_version="v1", call_id="ec_route_probe", max_output_tokens=0
        )
        decision = self._embedding_selector.select(self._catalog, probe)
        ordered = [decision.model.id]
        for model_id in decision.route.fallbacks:
            model = self._catalog.models.get(model_id)
            if model is None or not model.supports("embeddings") or model.embedding_dimensions is None:
                continue
            if model.id not in ordered:
                ordered.append(model.id)
        return tuple(ordered)

    def fulfill(
        self,
        request: intelligence_pb2.ModelCallRequest,
        *,
        deadline_ms: int | None = None,
        cancellation: CancellationToken | None = None,
        is_cancelled: Callable[[], bool] | None = None,
    ) -> Fulfillment:
        """Build a fulfilment, or raise a typed `GatewayError` before any provider call.

        `is_cancelled` is an additional caller-supplied liveness predicate (the gRPC servicer
        passes its context), so an abandoned stream is closed promptly even while the provider
        is silent.
        """
        prepared = self.prepare(request, deadline_ms=deadline_ms, cancellation=cancellation)
        token_id = request.cancellation_token
        registered = bool(token_id) and cancellation is None

        def on_terminal() -> None:
            if registered:
                self._cancellations.forget(token_id)

        return Fulfillment(
            factory=EventFactory(request.call_id, prepared.route.route_id),
            adapter=prepared.adapter,
            request=prepared.prepared_request.as_http_request(),
            transport=self._transport,
            cancellation=prepared.cancellation,
            deadline=time.monotonic() + (prepared.budget_ms / 1000.0),
            clock=self._clock,
            cost_class=prepared.route.model.cost_class,
            logger=self._logger,
            on_terminal=on_terminal,
            is_cancelled=is_cancelled,
        )

    def prepare(
        self,
        request: intelligence_pb2.ModelCallRequest,
        *,
        deadline_ms: int | None = None,
        cancellation: CancellationToken | None = None,
    ) -> PreparedCall:
        """Resolve route, custody, tools and the exact provider request; no provider call."""
        if not request.call_id.strip():
            raise GatewayError(GatewayErrorCode.VALIDATION_SCHEMA, "call_id is required")
        if request.timeout_ms <= 0:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA,
                "timeout_ms is required for a model call (DOMAIN.md §11.1)",
            )
        now = int(self._clock() * 1000)
        remaining_ms: int | None = None
        if deadline_ms is not None:
            remaining_ms = deadline_ms - now
            if remaining_ms <= 0:
                raise GatewayError(
                    GatewayErrorCode.TOOL_TIMEOUT,
                    "call deadline expired before the model call",
                    retryable=False,
                )
        budget_ms = request.timeout_ms if remaining_ms is None else min(request.timeout_ms, remaining_ms)

        route = self.resolve_route(request)
        # When a data policy is installed (INT-003), it is enforced before a byte is built for
        # transmission: a disallowed data/provider combination fails here, and what does leave is
        # redacted first.
        if self._dlp is not None:
            self._dlp_decision = self._dlp.check(request, route.model, route.provider)
            if self._dlp_decision.fired_redactions:
                self._dlp.redact(request)
        provider = route.provider
        if request.max_output_tokens > route.model.context_window:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_BOUNDS,
                f"max_output_tokens exceeds the {route.model.id} context window from the catalog",
            )
        base_url = provider.resolve_base_url(self._environ)
        credential = self._credentials.resolve(provider)
        tools = resolve_tools(request.tools, self._tool_schemas, route.model.capabilities)
        messages = parse_messages(request)
        adapter = adapter_for_kind(provider.kind)
        prepared_request = adapter.build_request(
            ProviderCallContext(
                call=request,
                route=route,
                credential=credential,
                tools=tools,
                messages=messages,
                base_url=base_url,
            )
        )
        # Re-assert the shared request shape on the final bytes: an adapter cannot smuggle a
        # provider compaction/context-editing feature or a fake operator turn past the guard.
        assert_no_disabled_features(prepared_request.body, prepared_request.headers)
        assert_operator_channel(messages, prepared_request.body_json())
        if cancellation is None:
            cancellation = (
                self._cancellations.for_token(request.cancellation_token)
                if request.cancellation_token
                else CancellationToken()
            )
        return PreparedCall(
            route=route,
            adapter=adapter,
            prepared_request=prepared_request,
            cancellation=cancellation,
            budget_ms=budget_ms,
        )

    def embed(
        self,
        texts: Sequence[str],
        *,
        model_hint: str = "",
        call_id: str = "",
        dlp_profile: str = "",
        timeout_ms: int = DEFAULT_EMBEDDING_TIMEOUT_MS,
        deadline_ms: int | None = None,
        is_cancelled: Callable[[], bool] | None = None,
    ) -> EmbeddingOutcome:
        """Embed `texts` through the `embedding` request class; vectors in input order.

        Ordering matches a chat call: the same deterministic route resolution, the same
        credential custody and the same DLP guard before a byte is built, then one bounded
        request over the pinned transport. Every failure is a typed `GatewayError`, so a caller
        sees the same taxonomy it sees from `FulfillModel`.

        Route resolution uses the embedding-class selector: the conversation selector is
        class-blind (it returns `routing.primary`), and an embedding route must be one that
        declares the `embeddings` capability and a width, not whatever model leads chat.
        """
        inputs = validate_embedding_inputs(texts)
        if timeout_ms <= 0:
            raise GatewayError(
                GatewayErrorCode.VALIDATION_SCHEMA, "timeout_ms is required for an embedding call"
            )
        now = int(self._clock() * 1000)
        remaining_ms: int | None = None
        if deadline_ms is not None:
            remaining_ms = deadline_ms - now
            if remaining_ms <= 0:
                raise GatewayError(
                    GatewayErrorCode.TOOL_TIMEOUT,
                    "call deadline expired before the embedding call",
                    retryable=False,
                )
        budget_ms = timeout_ms if remaining_ms is None else min(timeout_ms, remaining_ms)
        request = intelligence_pb2.ModelCallRequest(
            schema_version="v1",
            call_id=call_id.strip() or f"ec_{new_ulid()}",
            route_hint=model_hint.strip(),
            # The embedding request class's shape: no conversation, no output tokens.
            max_output_tokens=0,
            dlp_profile=dlp_profile,
            timeout_ms=budget_ms,
        )
        decision = self._embedding_selector.select(self._catalog, request)
        model = decision.model
        if model.embedding_dimensions is None:
            raise GatewayError(
                GatewayErrorCode.ROUTE_UNAVAILABLE,
                f"model '{model.id}' declares no embedding_dimensions, so its vectors cannot be "
                "checked against the index width",
            )
        if self._dlp is not None:
            # Same guard as a chat call, before anything is built for transmission: the data
            # class is refused here, and a redaction rule that fires is recorded.
            self._dlp_decision = self._dlp.check(request, model, decision.provider)
        base_url = decision.provider.resolve_base_url(self._environ)
        credential = self._credentials.resolve(decision.provider)
        wire = embedding_wire_for_kind(decision.provider.kind)
        prepared = wire.build_request(
            EmbeddingCallContext(
                model_catalog_id=model.id,
                wire_model_id=model.model_id,
                provider=decision.provider.name,
                credential=credential,
                base_url=base_url,
                texts=inputs,
                timeout_seconds=max(budget_ms / 1000.0, 0.001),
            )
        )
        body = self._post_embedding(
            prepared.as_http_request(),
            expected=len(inputs),
            dimensions=model.embedding_dimensions,
            is_cancelled=is_cancelled,
        )
        vectors = wire.decode(prepared, body)
        if len(vectors) != len(inputs):
            raise GatewayError(
                GatewayErrorCode.VALIDATION_BOUNDS,
                f"route {model.id} returned {len(vectors)} vectors for {len(inputs)} inputs",
            )
        return EmbeddingOutcome(
            model_catalog_id=model.id,
            wire_model_id=model.model_id,
            provider=decision.provider.name,
            route_id=decision.route_id,
            dimensions=check_width(vectors, expected=model.embedding_dimensions, model_catalog_id=model.id),
            vectors=vectors,
            chosen_by=decision.route.chosen_by,
        )

    def _post_embedding(
        self,
        request: HttpRequest,
        *,
        expected: int,
        dimensions: int,
        is_cancelled: Callable[[], bool] | None,
    ) -> bytes:
        """Send one embedding request and read its bounded body, or raise a typed error.

        The response is read through the same `StreamResponse` the streaming path uses, so the
        socket timeout, the call deadline and caller cancellation all still bound this call; a
        non-2xx status is classified by the shared provider-error mapping and never parsed as
        data, because an error body can echo the request.
        """
        budget = response_byte_budget(expected, dimensions)
        deadline = time.monotonic() + max(request.timeout_seconds, 0.001)
        cancelled = is_cancelled if is_cancelled is not None else _never_cancelled
        response: StreamResponse | None = None
        chunks: list[bytes] = []
        total = 0
        try:
            response = self._transport.open(request)
            status = response.status
            if status < 200 or status >= 300:
                result = http_error_result(status, b"")
                raise GatewayError(
                    GatewayErrorCode(result.error_code or str(GatewayErrorCode.PROVIDER_UNAVAILABLE)),
                    f"embedding provider answered HTTP {status}",
                    retryable=result.retryable,
                )
            while True:
                line = response.read_line(deadline=deadline, cancelled=cancelled)
                if line is None:
                    break
                total += len(line)
                if total > budget:
                    raise GatewayError(
                        GatewayErrorCode.VALIDATION_BOUNDS,
                        f"embedding response exceeded {budget} bytes for {expected} inputs",
                    )
                chunks.append(line)
            return b"".join(chunks)
        except StreamCancelledError as error:
            raise GatewayError(
                GatewayErrorCode.TOOL_TIMEOUT, "embedding call was cancelled", retryable=False
            ) from error
        except TransportTimeoutError as error:
            raise GatewayError(
                GatewayErrorCode.TOOL_TIMEOUT, "embedding provider did not answer in time", retryable=True
            ) from error
        except TransportError as error:
            raise GatewayError(
                GatewayErrorCode.PROVIDER_UNAVAILABLE, "embedding provider is unreachable", retryable=True
            ) from error
        finally:
            if response is not None:
                with contextlib.suppress(OSError):
                    response.close()


def _never_cancelled() -> bool:
    return False


class Fulfillment:
    """A single normalized `ModelEvent` stream plus its terminal outcome."""

    __slots__ = (
        "_adapter",
        "_cancellation",
        "_clock",
        "_closed",
        "_consumed",
        "_cost_class",
        "_deadline",
        "_decoder",
        "_events",
        "_external_cancelled",
        "_factory",
        "_logger",
        "_on_terminal",
        "_outcome",
        "_request",
        "_response",
        "_transport",
    )

    def __init__(
        self,
        *,
        factory: EventFactory,
        adapter: ProviderAdapter,
        request: HttpRequest,
        transport: HttpTransport,
        cancellation: CancellationToken,
        deadline: float,
        clock: Callable[[], float],
        cost_class: str,
        logger: logging.Logger,
        on_terminal: Callable[[], None],
        is_cancelled: Callable[[], bool] | None = None,
    ) -> None:
        self._factory = factory
        self._adapter = adapter
        self._request = request
        self._transport = transport
        self._cancellation = cancellation
        self._deadline = deadline
        self._clock = clock
        self._cost_class = cost_class
        self._logger = logger
        self._on_terminal = on_terminal
        self._external_cancelled = is_cancelled
        self._response: StreamResponse | None = None
        self._decoder: Iterator[object] | None = None
        self._consumed: ProviderResult | None = None
        self._outcome: TerminalOutcome | None = None
        self._closed = False
        self._events: Generator[intelligence_pb2.ModelEvent, None, None] = self._run()

    # -- public surface ------------------------------------------------------------

    @property
    def call_id(self) -> str:
        return self._factory.call_id

    @property
    def route_id(self) -> str:
        return self._factory.route_id

    @property
    def outcome(self) -> TerminalOutcome | None:
        """Terminal outcome once the stream ended, was cancelled, timed out or errored."""
        return self._outcome

    @property
    def closed(self) -> bool:
        return self._closed

    def __iter__(self) -> Iterator[intelligence_pb2.ModelEvent]:
        return self

    def __next__(self) -> intelligence_pb2.ModelEvent:
        return next(self._events)

    def cancel(self) -> None:
        """Signal cancellation; the next pull (or `close`) terminates the stream."""
        self._cancellation.cancel()

    def _cancelled(self) -> bool:
        if self._cancellation.is_cancelled():
            return True
        external = self._external_cancelled
        return bool(external()) if external is not None else False

    def close(self) -> None:
        """Terminate the stream now: cancel, close the provider socket, record the outcome."""
        if self._closed:
            return
        self._closed = True
        self._cancellation.cancel()
        self._events.close()
        self._close_transport()
        self._close_decoder()
        self._record_cancellation_if_unresolved()

    # -- stream --------------------------------------------------------------------

    def _run(self) -> Generator[intelligence_pb2.ModelEvent, None, None]:
        started_at = self._clock()
        factory = self._factory
        try:
            yield factory.started()
            try:
                yield from self._consume()
            except StreamCancelledError:
                self._consumed = ProviderResult(
                    stop_reason=intelligence_pb2.MODEL_STOP_REASON_CANCELLED,
                    usage=factory.partial_usage,
                )
            except TransportTimeoutError:
                self._consumed = ProviderResult(
                    stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
                    usage=factory.partial_usage,
                    error_code=str(GatewayErrorCode.TOOL_TIMEOUT),
                    retryable=True,
                )
            except TransportError:
                self._consumed = ProviderResult(
                    stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
                    usage=factory.partial_usage,
                    error_code=str(GatewayErrorCode.PROVIDER_UNAVAILABLE),
                    retryable=True,
                )
            except GatewayError as failure:
                self._consumed = ProviderResult(
                    stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
                    usage=factory.partial_usage,
                    error_code=str(failure.code),
                    retryable=failure.retryable,
                )
            result = self._consumed or ProviderResult(
                stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
                usage=factory.partial_usage,
                error_code=str(GatewayErrorCode.PROVIDER_UNAVAILABLE),
                retryable=True,
            )
            self._consumed = result
            if result.error_code:
                yield factory.error(result.error_code, retryable=result.retryable)
            yield factory.usage(result.usage)
            yield factory.stop(result.stop_reason)
            latency_ms = max(int((self._clock() - started_at) * 1000), 0)
            cost_minor_units = estimate_cost_minor_units(result.usage, self._cost_class)
            self._record(result, latency_ms=latency_ms, cost_minor_units=cost_minor_units)
            yield factory.completed(latency_ms=latency_ms, cost_estimate_minor_units=cost_minor_units)
        finally:
            self._close_transport()
            self._close_decoder()
            self._record_cancellation_if_unresolved()

    def _consume(self) -> Iterator[intelligence_pb2.ModelEvent]:
        response = self._transport.open(self._request)
        self._response = response
        if not 200 <= response.status < 300:
            self._consumed = http_error_result(response.status, response.read_body())
            return
        decoder = self._adapter.decode(self._factory, self._lines(response))
        self._decoder = decoder
        for item in decoder:
            if isinstance(item, ProviderResult):
                self._consumed = item
                return
            if self._cancelled():
                raise StreamCancelledError("cancelled between provider events")
            yield item
        if self._cancelled():
            raise StreamCancelledError("cancelled between provider events")
        self._consumed = ProviderResult(
            stop_reason=intelligence_pb2.MODEL_STOP_REASON_ERROR,
            usage=self._factory.partial_usage,
            error_code=str(GatewayErrorCode.PROVIDER_UNAVAILABLE),
            retryable=True,
        )

    def _lines(self, response: StreamResponse) -> Iterator[bytes]:
        while True:
            line = response.read_line(deadline=self._deadline, cancelled=self._cancellation.is_cancelled)
            if line is None:
                return
            yield line

    # -- bookkeeping ---------------------------------------------------------------

    def _close_transport(self) -> None:
        response = self._response
        self._response = None
        if response is not None:
            response.close()

    def _close_decoder(self) -> None:
        decoder = self._decoder
        self._decoder = None
        if decoder is None:
            return
        closer = getattr(decoder, "close", None)
        if callable(closer):
            closer()

    def _record(self, result: ProviderResult, *, latency_ms: int, cost_minor_units: int) -> None:
        if self._outcome is not None:
            return
        cancelled = result.stop_reason == intelligence_pb2.MODEL_STOP_REASON_CANCELLED
        self._outcome = TerminalOutcome(
            call_id=self._factory.call_id,
            route_id=self._factory.route_id,
            stop_reason=result.stop_reason,
            usage=result.usage,
            latency_ms=latency_ms,
            cost_estimate_minor_units=cost_minor_units,
            cancelled=cancelled,
            error_code=result.error_code,
            retryable=result.retryable,
        )
        self._on_terminal()
        self._logger.info(
            json.dumps(
                {
                    "event": "model_gateway.call_completed",
                    "call_id": self._factory.call_id,
                    "route_id": self._factory.route_id,
                    "stop_reason": int(result.stop_reason),
                    "error_code": result.error_code,
                    "retryable": result.retryable,
                    "cancelled": cancelled,
                    "latency_ms": latency_ms,
                    "input_tokens": result.usage.input_tokens,
                    "output_tokens": result.usage.output_tokens,
                    "cache_read_tokens": result.usage.cache_read_tokens,
                    "cache_write_tokens": result.usage.cache_write_tokens,
                },
                sort_keys=True,
            )
        )

    def _record_cancellation_if_unresolved(self) -> None:
        if self._outcome is not None:
            return
        self._record(
            ProviderResult(
                stop_reason=intelligence_pb2.MODEL_STOP_REASON_CANCELLED,
                usage=self._factory.partial_usage,
            ),
            latency_ms=0,
            cost_minor_units=0,
        )
