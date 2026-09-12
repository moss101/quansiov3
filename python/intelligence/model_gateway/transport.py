"""Streaming HTTP transport for provider calls.

The gateway talks to providers over plain HTTP/1.1 with server-sent events (SSE) or JSON
error bodies. `http.client` from the standard library is used deliberately: the intelligence
plane ships no provider SDK and no HTTP framework, so the wire is pinned, inspectable and
supplied by the catalog's `base_url`.

Every response is read by a worker thread that pushes lines into a queue. The consumer's
`read_line` polls that queue, so a cancelled call or an expired deadline stops the stream
immediately and `close()` tears the socket down, which terminates the reader. A provider that
accepts the connection and never answers is bounded by the socket timeout supplied in
`HttpRequest.timeout_seconds`.
"""

from __future__ import annotations

import contextlib
import http.client
import queue
import threading
import time
import urllib.parse
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Protocol

from intelligence.model_gateway.credentials import SecretValue

# Queue poll interval: the upper bound on how long a cancel/deadline takes to be observed.
POLL_INTERVAL_SECONDS = 0.02
# Maximum provider error body kept for typed error mapping; the body is never logged.
MAX_ERROR_BODY_BYTES = 16_384


class TransportError(RuntimeError):
    """The provider endpoint could not be reached or the response failed at the socket level."""


class TransportTimeoutError(TimeoutError):
    """The provider did not answer, or did not finish answering, within the call budget."""


class StreamCancelledError(RuntimeError):
    """The caller cancelled the stream (or the deadline passed) while reading it."""


@dataclass(frozen=True, slots=True)
class HttpRequest:
    """One provider HTTP call; header values may be `SecretValue` so `repr` stays redacted."""

    method: str
    url: str
    headers: Mapping[str, str | SecretValue]
    body: bytes
    timeout_seconds: float

    def resolved_headers(self) -> dict[str, str]:
        """Header map with secret values revealed, built only at send time."""
        return {
            name: value.reveal() if isinstance(value, SecretValue) else value
            for name, value in self.headers.items()
        }

    def __repr__(self) -> str:
        # Never a body, never a revealed header value.
        return (
            f"HttpRequest(method={self.method!r}, url={self.url!r}, "
            f"headers={sorted(self.headers)}, body_bytes={len(self.body)}, "
            f"timeout_seconds={self.timeout_seconds!r})"
        )


class StreamResponse(Protocol):
    """A bounded, cancellable provider response."""

    @property
    def status(self) -> int: ...

    def read_line(self, *, deadline: float, cancelled: Callable[[], bool]) -> bytes | None:
        """Next response line without its terminator, or None at end of stream."""
        ...

    def read_body(self, limit: int = MAX_ERROR_BODY_BYTES) -> bytes:
        """Remaining body bytes (used for non-2xx error bodies only)."""
        ...

    def close(self) -> None: ...


class HttpTransport(Protocol):
    """Opens provider connections; injectable so tests can substitute a transport double."""

    def open(self, request: HttpRequest) -> StreamResponse: ...


class _StdlibResponse:
    """`StreamResponse` backed by an `http.client` response and a reader thread."""

    __slots__ = ("_closed", "_connection", "_queue", "_response", "_thread")

    def __init__(self, connection: http.client.HTTPConnection, response: http.client.HTTPResponse) -> None:
        self._connection = connection
        self._response = response
        self._queue: queue.Queue[tuple[str, bytes | BaseException | None]] = queue.Queue()
        self._closed = False
        self._thread = threading.Thread(target=self._pump, name="model-gateway-provider-reader", daemon=True)
        self._thread.start()

    @property
    def status(self) -> int:
        return int(self._response.status)

    def _pump(self) -> None:
        try:
            while True:
                line = self._response.readline()
                if not line:
                    self._queue.put(("eof", None))
                    return
                self._queue.put(("line", line))
        except TimeoutError:  # socket timeout mid-stream
            self._queue.put(("error", TransportTimeoutError("provider stream stalled")))
        except OSError:
            self._queue.put(("error", TransportError("provider stream failed")))
        except AttributeError:
            # http.client raises AttributeError when a readline lands after the peer closed a
            # `Connection: close` response; that is end-of-stream, not a provider failure.
            self._queue.put(("eof", None))

    def read_line(self, *, deadline: float, cancelled: Callable[[], bool]) -> bytes | None:
        while True:
            if cancelled():
                raise StreamCancelledError("provider stream cancelled by caller")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TransportTimeoutError("provider stream exceeded the call deadline")
            try:
                kind, payload = self._queue.get(timeout=min(POLL_INTERVAL_SECONDS, remaining))
            except queue.Empty:
                continue
            if kind == "line" and isinstance(payload, bytes):
                return payload.rstrip(b"\r\n")
            if kind == "eof":
                return None
            if isinstance(payload, BaseException):
                raise payload
            return None

    def read_body(self, limit: int = MAX_ERROR_BODY_BYTES) -> bytes:
        chunks: list[bytes] = []
        total = 0
        while total < limit:
            try:
                kind, payload = self._queue.get(timeout=0.05)
            except queue.Empty:
                break
            if kind == "line" and isinstance(payload, bytes):
                chunks.append(payload)
                total += len(payload)
                continue
            if kind == "error" and isinstance(payload, BaseException):
                break
            if kind == "eof":
                break
        return b"".join(chunks)[:limit]

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        for closer in (self._response.close, self._connection.close):
            try:
                closer()
            except OSError:  # already gone; cancellation is best-effort at the socket
                continue
        self._thread.join(timeout=1.0)


class StdlibHttpTransport:
    """`HttpTransport` over `http.client`, one connection per call."""

    __slots__ = ()

    def open(self, request: HttpRequest) -> StreamResponse:
        parts = urllib.parse.urlsplit(request.url)
        if parts.scheme not in {"http", "https"} or not parts.hostname:
            raise TransportError(f"provider base URL {request.url!r} is not an http(s) endpoint")
        connection_class = (
            http.client.HTTPSConnection if parts.scheme == "https" else http.client.HTTPConnection
        )
        connection = connection_class(parts.hostname, parts.port, timeout=max(request.timeout_seconds, 0.001))
        path = parts.path or "/"
        if parts.query:
            path = f"{path}?{parts.query}"
        try:
            connection.request(request.method, path, body=request.body, headers=request.resolved_headers())
            response = connection.getresponse()
        except (TimeoutError, OSError) as error:
            with contextlib.suppress(OSError):
                connection.close()
            if isinstance(error, TimeoutError):
                raise TransportTimeoutError("provider did not respond within the call budget") from error
            raise TransportError("provider connection failed") from error
        return _StdlibResponse(connection, response)
