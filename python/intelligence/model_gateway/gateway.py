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

import json
import logging
import os
import threading
import time
from collections.abc import Callable, Generator, Iterator, Mapping
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
from intelligence.model_gateway.errors import GatewayError, GatewayErrorCode
from intelligence.model_gateway.events import (
    EventFactory,
    TerminalOutcome,
    estimate_cost_minor_units,
)
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
