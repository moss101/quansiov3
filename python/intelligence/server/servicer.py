"""Implementation of the generated `quansio.v1.intelligence.IntelligenceGateway` contract.

Python proposes; the trusted Rust runtime commits (DOSSIER.md §3). Every method is a pure
proposal read: it validates the call scope, computes a result or returns a typed failure, and
mutates no canonical runtime, graph, effect or machine state. There is deliberately no method
here that writes one — the servicer's callable surface is exactly the generated RPC set plus
lifecycle bookkeeping, and `python/tests/intelligence/test_generated_binding_boundary.py`
asserts that.

Implemented today:
  * `ClassifyTrust` — deterministic, non-LLM trust labelling and injection heuristics
    (DOMAIN.md §12) from `intelligence.trust.classifier`; makes no model call.
  * `FulfillModel` — delegates to the model gateway (`intelligence.model_gateway`, INT-002)
    when a route resolves. The scope/deadline gate runs first and the gateway owns routing,
    credential custody, cancellation, timeout, usage and typed errors; this servicer only
    forwards the normalized `ModelEvent` stream. When no gateway is configured, or the
    catalog cannot resolve a route, the call fails closed with a typed error (DOMAIN.md §15)
    and no provider call is made. INT-003 owns routing policy and DLP.

Not implemented here; each returns a typed UNIMPLEMENTED failure naming the owning task that
will implement the behaviour (never a success-shaped empty response):
  * `BuildContext`  → INT-005 (ContextProjection assembly)
  * `Search`        → INT-005 (typed SearchProgram execution)
  * `ProposeMemory` → INT-007 (semantic memory candidates)
  * `Embed`         → INT-011 (embedding pipeline)
  * `Evaluate`      → INT-010 (evaluation harness)
INT-006 (Knowledge Fabric) and INT-012 (trust enforcement) have no RPC in the contract today.

`INTELLIGENCE_TASK_ID` / `_UNIMPLEMENTED_OWNERS` are the only source of that mapping; the
tests read it instead of restating it.
"""

from __future__ import annotations

import logging
import threading
import time
from collections.abc import Callable, Iterator, Mapping, Sequence
from typing import ClassVar, NoReturn

import grpc
from quansio.v1.intelligence import intelligence_pb2, service_pb2

from intelligence.model_gateway import Fulfillment, GatewayError, ModelGateway
from intelligence.server.errors import ErrorCode, ErrorDetail, ErrorEnvelope, abort_with
from intelligence.server.scope import CallScope, ScopeViolationError, validate_call_scope
from intelligence.trust.classifier import TrustClassification, classify

INTELLIGENCE_TASK_ID = "INT-001"
SCHEMA_VERSION = "v1"

SERVICER_LOGGER_NAME = "intelligence.server"

# Behaviour owned by a later task; the value is that task's id.
_UNIMPLEMENTED_OWNERS: Mapping[str, str] = {
    "BuildContext": "INT-005",
    "Search": "INT-005",
    "ProposeMemory": "INT-007",
    "Embed": "INT-011",
    "Evaluate": "INT-010",
}

IMPLEMENTED_METHODS: frozenset[str] = frozenset({"ClassifyTrust", "FulfillModel"})

TrustClassifier = Callable[[str, str], TrustClassification]


class IntelligenceGatewayServicer:
    """Serves `IntelligenceGateway`; proposals only, no canonical state mutation."""

    UNIMPLEMENTED_OWNERS: ClassVar[Mapping[str, str]] = _UNIMPLEMENTED_OWNERS
    IMPLEMENTED: ClassVar[frozenset[str]] = IMPLEMENTED_METHODS

    def __init__(
        self,
        *,
        trust_classifier: TrustClassifier = classify,
        clock: Callable[[], float] = time.time,
        logger: logging.Logger | None = None,
        gateway: ModelGateway | None = None,
    ) -> None:
        self._trust_classifier = trust_classifier
        self._clock = clock
        self._logger = logger if logger is not None else logging.getLogger(SERVICER_LOGGER_NAME)
        self._gateway = gateway
        self._idle = threading.Condition(threading.Lock())
        self._in_flight = 0
        self._draining = False

    # -- lifecycle -----------------------------------------------------------------

    @property
    def in_flight(self) -> int:
        with self._idle:
            return self._in_flight

    def begin_drain(self) -> None:
        """Refuse new calls; calls already accepted keep running."""
        with self._idle:
            self._draining = True

    def wait_for_idle(self, timeout: float) -> bool:
        """Block until no call is in flight; False when `timeout` seconds elapse first."""
        deadline = time.monotonic() + max(timeout, 0.0)
        with self._idle:
            while self._in_flight > 0:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                self._idle.wait(remaining)
            return True

    # -- call bookkeeping ----------------------------------------------------------

    def _begin_call(self, context: grpc.ServicerContext) -> None:
        with self._idle:
            if self._draining:
                abort_with(
                    context,
                    ErrorEnvelope(
                        code=ErrorCode.INTERNAL,
                        message="intelligence gateway is draining and refuses new calls",
                        details=(ErrorDetail("phase", "draining"),),
                    ),
                    grpc.StatusCode.UNAVAILABLE,
                )
            self._in_flight += 1

    def _end_call(self) -> None:
        with self._idle:
            self._in_flight -= 1
            if self._in_flight <= 0:
                self._idle.notify_all()

    def _now_ms(self) -> int:
        return int(self._clock() * 1000)

    def _validated_scope(self, method: str, request: object, context: grpc.ServicerContext) -> CallScope:
        invocation_metadata: Sequence[tuple[str, str | bytes]] = context.invocation_metadata() or ()
        try:
            return validate_call_scope(request, invocation_metadata, context, now_ms=self._now_ms())
        except ScopeViolationError as violation:
            # Log identifiers only: request content is never logged.
            self._logger.warning(
                "intelligence scope rejected method=%s code=%s correlation_id=%s",
                method,
                violation.envelope.code,
                violation.envelope.correlation_id,
            )
            abort_with(context, violation.envelope, violation.status)

    def _ensure_live(self, method: str, context: grpc.ServicerContext) -> None:
        if context.is_active():
            return
        self._logger.warning("intelligence call cancelled method=%s", method)
        abort_with(
            context,
            ErrorEnvelope(
                code=ErrorCode.INTERNAL,
                message="call cancelled before completion",
                details=(ErrorDetail("grpc_status", "CANCELLED"),),
            ),
            grpc.StatusCode.CANCELLED,
        )

    def _reject_unimplemented(self, method: str, request: object, context: grpc.ServicerContext) -> NoReturn:
        """Validate scope, then fail with a typed UNIMPLEMENTED naming the owning task."""
        self._validated_scope(method, request, context)
        owner = _UNIMPLEMENTED_OWNERS[method]
        abort_with(
            context,
            ErrorEnvelope(
                code=ErrorCode.INTERNAL,
                message=f"{method} is not implemented by {INTELLIGENCE_TASK_ID}; owned by {owner}",
                details=(
                    ErrorDetail("grpc_status", "UNIMPLEMENTED"),
                    ErrorDetail("owner_task", owner),
                    ErrorDetail("implemented_by", INTELLIGENCE_TASK_ID),
                ),
            ),
            grpc.StatusCode.UNIMPLEMENTED,
        )

    # -- implemented RPCs ----------------------------------------------------------

    def ClassifyTrust(
        self, request: service_pb2.TrustClassifyRequest, context: grpc.ServicerContext
    ) -> service_pb2.TrustClassifyResponse:
        """Deterministic trust label and injection verdict for one content segment."""
        self._begin_call(context)
        try:
            self._validated_scope("ClassifyTrust", request, context)
            self._ensure_live("ClassifyTrust", context)
            classification = self._trust_classifier(request.source_kind, request.content)
            self._ensure_live("ClassifyTrust", context)
            return service_pb2.TrustClassifyResponse(
                schema_version=SCHEMA_VERSION,
                trust_level=int(classification.trust_level),
                injection_suspected=classification.injection_suspected,
                matched_patterns=classification.matched_patterns,
            )
        finally:
            self._end_call()

    # -- RPCs owned by a later task ------------------------------------------------

    def FulfillModel(
        self, request: intelligence_pb2.ModelCallRequest, context: grpc.ServicerContext
    ) -> Iterator[intelligence_pb2.ModelEvent]:
        """Delegate a model call to the gateway; proposals only, no runtime authority.

        The INT-001 scope/deadline gate runs before anything else. Pre-stream failures
        (no gateway, unresolvable route, missing credential material, unusable request) abort
        the RPC with a typed DOMAIN.md §15 error. Failures after the stream started arrive as
        typed `KIND_ERROR` events, and the terminal outcome is recorded by the gateway.
        """
        self._begin_call(context)
        try:
            scope = self._validated_scope("FulfillModel", request, context)
            self._ensure_live("FulfillModel", context)
            fulfillment = self._start_fulfilment(request, scope, context)
        except BaseException:
            self._end_call()
            raise
        return self._stream_fulfilment(fulfillment, context)

    def _start_fulfilment(
        self,
        request: intelligence_pb2.ModelCallRequest,
        scope: CallScope,
        context: grpc.ServicerContext,
    ) -> Fulfillment:
        gateway = self._gateway
        if gateway is None:
            abort_with(
                context,
                ErrorEnvelope(
                    code=ErrorCode.ROUTE_UNAVAILABLE,
                    message="no model gateway is configured for this intelligence process",
                    correlation_id=scope.correlation_id,
                    details=(ErrorDetail("owner_task", "INT-002"),),
                ),
                grpc.StatusCode.FAILED_PRECONDITION,
            )
        try:
            return gateway.fulfill(
                request,
                deadline_ms=scope.deadline_ms,
                # `context.is_active()` flips once gRPC observes the client cancel; it is a
                # useful second signal but is not sufficient while a provider is silent, so
                # the canonical cancellation path stays `ModelCallRequest.cancellation_token`
                # (model_gateway.ModelGateway.cancel).
                is_cancelled=lambda: not context.is_active(),
            )
        except GatewayError as failure:
            abort_with(
                context,
                ErrorEnvelope(
                    code=_error_code(failure.code),
                    message=failure.message,
                    correlation_id=scope.correlation_id,
                    retryable=failure.retryable,
                    details=(ErrorDetail("owner_task", "INT-002"),),
                ),
            )

    def _stream_fulfilment(
        self, fulfillment: Fulfillment, context: grpc.ServicerContext
    ) -> Iterator[intelligence_pb2.ModelEvent]:
        try:
            for event in fulfillment:
                if not context.is_active():
                    # The caller went away: close the provider stream and record the outcome.
                    self._logger.warning(
                        "intelligence call cancelled mid-stream method=FulfillModel call_id=%s",
                        fulfillment.call_id,
                    )
                    break
                yield event
        finally:
            try:
                fulfillment.close()
            finally:
                self._end_call()

    def BuildContext(
        self, request: service_pb2.ContextBuildRequest, context: grpc.ServicerContext
    ) -> intelligence_pb2.ContextProjection:
        self._begin_call(context)
        try:
            self._reject_unimplemented("BuildContext", request, context)
        finally:
            self._end_call()

    def Search(
        self, request: intelligence_pb2.SearchProgram, context: grpc.ServicerContext
    ) -> service_pb2.SearchResultSet:
        self._begin_call(context)
        try:
            self._reject_unimplemented("Search", request, context)
        finally:
            self._end_call()

    def ProposeMemory(
        self, request: service_pb2.MemoryCandidate, context: grpc.ServicerContext
    ) -> service_pb2.ProposalAck:
        self._begin_call(context)
        try:
            self._reject_unimplemented("ProposeMemory", request, context)
        finally:
            self._end_call()

    def Embed(
        self, request: service_pb2.EmbedRequest, context: grpc.ServicerContext
    ) -> service_pb2.EmbedResponse:
        self._begin_call(context)
        try:
            self._reject_unimplemented("Embed", request, context)
        finally:
            self._end_call()

    def Evaluate(
        self, request: service_pb2.EvaluationRequest, context: grpc.ServicerContext
    ) -> service_pb2.EvaluationReport:
        self._begin_call(context)
        try:
            self._reject_unimplemented("Evaluate", request, context)
        finally:
            self._end_call()


def _error_code(code: str) -> ErrorCode:
    """Map a canonical DOMAIN.md §15 gateway code onto the boundary error enum."""
    try:
        return ErrorCode(code)
    except ValueError:  # pragma: no cover - the gateway emits taxonomy codes only
        return ErrorCode.INTERNAL
