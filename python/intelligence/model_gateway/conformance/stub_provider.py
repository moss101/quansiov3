"""Deterministic offline provider stub used by the gateway conformance suite.

It implements both wire formats the gateway normalizes — Anthropic Messages SSE
(`POST /v1/messages`) and OpenAI chat-completions SSE (`POST /v1/chat/completions`) — so the
three adapters are exercised through real HTTP and real SSE framing, with no network access
and no randomness. Responses are a pure function of (path, request body): the scenario is
selected by a `[[scenario:<name>]]` marker the caller places in a rendered message, and the
tool name and arguments come from the request itself.

This stub is test infrastructure. It is **never** real-boundary evidence for INT-002: the live
provider suite in `python/tests/intelligence/test_model_gateway_live.py` is the only
real-boundary proof, and it is gated on `QUANSIO_TEST_ANTHROPIC_API_KEY` /
`QUANSIO_TEST_OPENAI_API_KEY`.
"""

from __future__ import annotations

import json
import socket
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, cast

SCENARIO_MARKER_OPEN = "[[scenario:"
SCENARIO_MARKER_CLOSE = "]]"

# Short names are surfaced in the marker; "usage_repeat" exists to prove that repeated usage
# payloads are merged rather than summed.
SCENARIOS: frozenset[str] = frozenset(
    {
        "text",
        "usage_repeat",
        "tool_call",
        "thinking",
        "refusal",
        "max_tokens",
        "stream_error",
        "no_stop",
        "http_error_retryable",
        "http_error_fatal",
        "slow",
    }
)

_ANTHROPIC_PATH = "/v1/messages"
_OPENAI_PATH = "/v1/chat/completions"
_TOOL_ARG_CHUNKS = ('{"value":', '"42"}')
_SLOW_HEARTBEATS = 100


@dataclass(frozen=True, slots=True)
class RecordedCall:
    """One request the stub received; `repr` keeps the body out of test failure output."""

    path: str
    headers: dict[str, str]
    body: bytes
    scenario: str

    def json(self) -> dict[str, Any]:
        decoded = json.loads(self.body or b"{}")
        return decoded if isinstance(decoded, dict) else {}

    def __repr__(self) -> str:
        return (
            f"RecordedCall(path={self.path!r}, scenario={self.scenario!r}, "
            f"headers={sorted(self.headers)}, body_bytes={len(self.body)})"
        )


@dataclass(slots=True)
class StubState:
    """Observation surface for the tests: requests seen and stream (dis)connects."""

    calls: list[RecordedCall] = field(default_factory=list)
    active_streams: int = 0
    opened_streams: int = 0
    closed_streams: int = 0
    disconnected_streams: int = 0
    lock: threading.Lock = field(default_factory=threading.Lock)

    def record(self, call: RecordedCall) -> None:
        with self.lock:
            self.calls.append(call)

    def stream_opened(self) -> None:
        with self.lock:
            self.active_streams += 1
            self.opened_streams += 1

    def stream_closed(self, *, disconnected: bool) -> None:
        with self.lock:
            self.active_streams = max(self.active_streams - 1, 0)
            if disconnected:
                self.disconnected_streams += 1
            else:
                self.closed_streams += 1


class _StubServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self) -> None:
        super().__init__(("127.0.0.1", 0), _StubHandler)
        self.state = StubState()


class StubProvider:
    """A loopback stub provider; start it for a test and inspect `state` afterwards."""

    def __init__(self) -> None:
        self._server = _StubServer()
        self._thread = threading.Thread(
            target=self._server.serve_forever, name="conformance-stub-provider", daemon=True
        )

    @property
    def state(self) -> StubState:
        return self._server.state

    @property
    def base_url(self) -> str:
        address = self._server.server_address
        return f"http://{cast(str, address[0])}:{address[1]}"

    def __enter__(self) -> StubProvider:
        self._thread.start()
        return self

    def __exit__(self, *_exc: object) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5.0)


class NonRespondingProvider:
    """Accepts TCP connections and never answers; used only to prove the timeout path."""

    def __init__(self) -> None:
        self._socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._socket.bind(("127.0.0.1", 0))
        self._socket.listen(8)
        self._accepted: list[socket.socket] = []
        self._stop = threading.Event()
        self._thread = threading.Thread(
            target=self._accept_forever, name="conformance-silent-provider", daemon=True
        )

    @property
    def base_url(self) -> str:
        address = self._socket.getsockname()
        return f"http://{cast(str, address[0])}:{address[1]}"

    def _accept_forever(self) -> None:
        self._socket.settimeout(0.2)
        while not self._stop.is_set():
            try:
                connection, _address = self._socket.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            self._accepted.append(connection)

    def __enter__(self) -> NonRespondingProvider:
        self._thread.start()
        return self

    def __exit__(self, *_exc: object) -> None:
        self._stop.set()
        for connection in self._accepted:
            try:
                connection.close()
            except OSError:  # pragma: no cover - already closed by the gateway
                continue
        self._socket.close()
        self._thread.join(timeout=5.0)


class _StubHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 - stdlib signature
        return  # the stub is silent: tests assert on captured requests, not stderr

    # -- plumbing ------------------------------------------------------------------

    @property
    def _state(self) -> StubState:
        return cast(_StubServer, self.server).state

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0") or "0")
        body = self.rfile.read(length) if length else b""
        headers = {name.lower(): value for name, value in self.headers.items()}
        scenario = _scenario_of(body)
        self._state.record(RecordedCall(self.path, headers, body, scenario))
        if not _authorized(self.path, headers):
            self._send_json(401, {"error": {"type": "authentication_error", "message": "no key"}})
            return
        if scenario == "http_error_retryable":
            self._send_json(429, {"error": {"type": "rate_limit_error", "message": "slow down"}})
            return
        if scenario == "http_error_fatal":
            self._send_json(400, {"error": {"type": "invalid_request_error", "message": "bad"}})
            return
        if self.path == _ANTHROPIC_PATH:
            self._anthropic(body, scenario)
        elif self.path == _OPENAI_PATH:
            self._openai(body, scenario)
        else:
            self._send_json(404, {"error": {"type": "invalid_request_error", "message": "no route"}})

    def do_GET(self) -> None:
        if self.path == "/healthz":
            self._send_json(200, {"status": "ok"})
        else:
            self._send_json(404, {"error": {"type": "invalid_request_error", "message": "no route"}})

    def _send_json(self, status: int, payload: dict[str, Any]) -> None:
        data = json.dumps(payload, sort_keys=True).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def _begin_stream(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        self._state.stream_opened()

    def _write(self, chunk: str) -> bool:
        try:
            self.wfile.write(chunk.encode("utf-8"))
            self.wfile.flush()
            return True
        except OSError:
            return False

    def _sse(self, events: list[str]) -> None:
        for chunk in events:
            if not self._write(chunk):
                self._state.stream_closed(disconnected=True)
                return
        self._state.stream_closed(disconnected=False)

    # -- Anthropic Messages --------------------------------------------------------

    def _anthropic(self, body: bytes, scenario: str) -> None:
        request = _decode(body)
        model = str(request.get("model", ""))
        tools = request.get("tools")
        tool_name = "unknown.tool"
        if isinstance(tools, list) and tools and isinstance(tools[0], dict):
            tool_name = str(tools[0].get("name", tool_name))
        self._begin_stream()
        if scenario == "stream_error":
            self._sse(
                [
                    _event("message_start", _anthropic_start(model)),
                    _event(
                        "error",
                        {
                            "type": "error",
                            "error": {"type": "overloaded_error", "message": "overloaded"},
                        },
                    ),
                ]
            )
            return
        if scenario == "slow":
            first = _event(
                "content_block_delta",
                {
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "Hello "},
                },
            )
            if not self._write(_event("message_start", _anthropic_start(model))):
                self._state.stream_closed(disconnected=True)
                return
            if not self._write(
                _event(
                    "content_block_start",
                    {"type": "content_block_start", "index": 0, "content_block": {"type": "text"}},
                )
            ):
                self._state.stream_closed(disconnected=True)
                return
            if not self._write(first):
                self._state.stream_closed(disconnected=True)
                return
            for _ in range(_SLOW_HEARTBEATS):
                time.sleep(0.05)
                if not self._write(": keep-alive\n\n"):
                    self._state.stream_closed(disconnected=True)
                    return
            self._sse(_anthropic_tail("end_turn"))
            return

        events: list[str] = [_event("message_start", _anthropic_start(model))]
        if scenario == "thinking":
            events += [
                _event(
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "thinking", "thinking": ""},
                    },
                ),
                _event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "thinking_delta", "thinking": "weighing options"},
                    },
                ),
                _event("content_block_stop", {"type": "content_block_stop", "index": 0}),
                _event(
                    "content_block_start",
                    {"type": "content_block_start", "index": 1, "content_block": {"type": "text"}},
                ),
            ]
        if scenario == "tool_call":
            events += [
                _event(
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "tool_use", "id": "toolu_stub_1", "name": tool_name},
                    },
                ),
                _event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "input_json_delta", "partial_json": _TOOL_ARG_CHUNKS[0]},
                    },
                ),
                _event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "input_json_delta", "partial_json": _TOOL_ARG_CHUNKS[1]},
                    },
                ),
                _event("content_block_stop", {"type": "content_block_stop", "index": 0}),
            ]
        else:
            index = 1 if scenario == "thinking" else 0
            events += [
                _event(
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "text", "text": ""},
                    },
                ),
                _event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "text_delta", "text": "Hello "},
                    },
                ),
                _event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "text_delta", "text": "world"},
                    },
                ),
                _event("content_block_stop", {"type": "content_block_stop", "index": index}),
            ]
        if scenario == "no_stop":
            events.append(_event("message_stop", {"type": "message_stop"}))
            self._sse(events)
            return
        stop_reason = {
            "tool_call": "tool_use",
            "refusal": "refusal",
            "max_tokens": "max_tokens",
        }.get(scenario, "end_turn")
        events += _anthropic_tail(stop_reason, repeat_usage=scenario == "usage_repeat")
        self._sse(events)

    # -- OpenAI chat completions ---------------------------------------------------

    def _openai(self, body: bytes, scenario: str) -> None:
        request = _decode(body)
        model = str(request.get("model", ""))
        tools = request.get("tools")
        tool_name = "unknown.tool"
        if isinstance(tools, list) and tools and isinstance(tools[0], dict):
            function = tools[0].get("function")
            if isinstance(function, dict):
                tool_name = str(function.get("name", tool_name))
        self._begin_stream()
        if scenario == "stream_error":
            self._sse(
                [
                    _data(
                        {
                            "id": "chatcmpl-stub",
                            "object": "chat.completion.chunk",
                            "model": model,
                            "choices": [],
                        }
                    ),
                    _data({"error": {"type": "server_error", "message": "upstream failed"}}),
                ]
            )
            return
        if scenario == "slow":
            for chunk in _openai_head(model):
                if not self._write(chunk):
                    self._state.stream_closed(disconnected=True)
                    return
            for _ in range(_SLOW_HEARTBEATS):
                time.sleep(0.05)
                if not self._write(": keep-alive\n\n"):
                    self._state.stream_closed(disconnected=True)
                    return
            self._sse(_openai_tail(model, "stop"))
            return

        events: list[str] = []
        if scenario == "thinking":
            events.append(_data(_openai_chunk(model, {"reasoning_content": "weighing options"})))
        if scenario == "tool_call":
            events.append(
                _data(
                    {
                        "id": "chatcmpl-stub",
                        "object": "chat.completion.chunk",
                        "model": model,
                        "choices": [
                            {
                                "index": 0,
                                "delta": {
                                    "tool_calls": [
                                        {
                                            "index": 0,
                                            "id": "call_stub_1",
                                            "type": "function",
                                            "function": {"name": tool_name, "arguments": _TOOL_ARG_CHUNKS[0]},
                                        }
                                    ]
                                },
                                "finish_reason": None,
                            }
                        ],
                    }
                )
            )
            events.append(
                _data(
                    {
                        "id": "chatcmpl-stub",
                        "object": "chat.completion.chunk",
                        "model": model,
                        "choices": [
                            {
                                "index": 0,
                                "delta": {
                                    "tool_calls": [
                                        {"index": 0, "function": {"arguments": _TOOL_ARG_CHUNKS[1]}}
                                    ]
                                },
                                "finish_reason": None,
                            }
                        ],
                    }
                )
            )
        elif scenario == "refusal":
            events.append(_data(_openai_chunk(model, {"refusal": "I cannot help with that."})))
        else:
            events += _openai_head(model)
        if scenario == "no_stop":
            events.append(_data(_openai_chunk(model, {})))
            events.append("data: [DONE]\n\n")
            self._sse(events)
            return
        finish = {
            "tool_call": "tool_calls",
            "refusal": "stop",
            "max_tokens": "length",
        }.get(scenario, "stop")
        events += _openai_tail(model, finish, repeat_usage=scenario == "usage_repeat")
        self._sse(events)


def _anthropic_start(model: str) -> dict[str, Any]:
    return {
        "type": "message_start",
        "message": {
            "id": "msg_stub",
            "type": "message",
            "role": "assistant",
            "model": model,
            "usage": {
                "input_tokens": 11,
                "output_tokens": 0,
                "cache_read_input_tokens": 3,
                "cache_creation_input_tokens": 2,
            },
        },
    }


def _anthropic_tail(stop_reason: str, *, repeat_usage: bool = False) -> list[str]:
    usage = {
        "input_tokens": 11,
        "output_tokens": 7,
        "cache_read_input_tokens": 3,
        "cache_creation_input_tokens": 2,
    }
    events = [
        _event(
            "message_delta",
            {"type": "message_delta", "delta": {"stop_reason": stop_reason}, "usage": usage},
        )
    ]
    if repeat_usage:
        events.append(
            _event(
                "message_delta",
                {"type": "message_delta", "delta": {}, "usage": usage},
            )
        )
    events.append(_event("message_stop", {"type": "message_stop"}))
    return events


def _openai_head(model: str) -> list[str]:
    return [
        _data(_openai_chunk(model, {"role": "assistant", "content": "Hello "})),
        _data(_openai_chunk(model, {"content": "world"})),
    ]


def _openai_tail(model: str, finish_reason: str, *, repeat_usage: bool = False) -> list[str]:
    usage = {
        "prompt_tokens": 11,
        "completion_tokens": 7,
        "total_tokens": 18,
        "prompt_tokens_details": {"cached_tokens": 3},
    }
    events = [
        _data(
            {
                "id": "chatcmpl-stub",
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}],
            }
        ),
        _data(
            {
                "id": "chatcmpl-stub",
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [],
                "usage": usage,
            }
        ),
    ]
    if repeat_usage:
        events.append(
            _data(
                {
                    "id": "chatcmpl-stub",
                    "object": "chat.completion.chunk",
                    "model": model,
                    "choices": [],
                    "usage": usage,
                }
            )
        )
    events.append("data: [DONE]\n\n")
    return events


def _openai_chunk(model: str, delta: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": "chatcmpl-stub",
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": None}],
    }


def _event(name: str, payload: dict[str, Any]) -> str:
    return f"event: {name}\ndata: {json.dumps(payload, sort_keys=True)}\n\n"


def _data(payload: dict[str, Any]) -> str:
    return f"data: {json.dumps(payload, sort_keys=True)}\n\n"


def _decode(body: bytes) -> dict[str, Any]:
    try:
        decoded = json.loads(body or b"{}")
    except json.JSONDecodeError:
        return {}
    return decoded if isinstance(decoded, dict) else {}


def _scenario_of(body: bytes) -> str:
    request = _decode(body)
    marker = _find_marker(request)
    return marker if marker in SCENARIOS else "text"


def _find_marker(node: object) -> str:
    if isinstance(node, str):
        start = node.find(SCENARIO_MARKER_OPEN)
        if start >= 0:
            end = node.find(SCENARIO_MARKER_CLOSE, start)
            if end > start:
                return node[start + len(SCENARIO_MARKER_OPEN) : end]
        return ""
    if isinstance(node, dict):
        for value in node.values():
            found = _find_marker(value)
            if found:
                return found
        return ""
    if isinstance(node, list):
        for item in node:
            found = _find_marker(item)
            if found:
                return found
    return ""


def _authorized(path: str, headers: dict[str, str]) -> bool:
    if path == _ANTHROPIC_PATH:
        return bool(headers.get("x-api-key", "").strip())
    return headers.get("authorization", "").startswith("Bearer ")
