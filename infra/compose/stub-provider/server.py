"""Deterministic stub model provider for offline development (GOV-007).

Implements the minimal OpenAI-compatible surface the model gateway uses in dev:
  GET  /healthz              liveness for compose healthcheck and scripts/dev/healthcheck
  GET  /v1/models            lists the single local stub model
  POST /v1/chat/completions  returns a deterministic completion for the request; with
                             `"stream": true` (what the gateway always sends) the same
                             completion as server-sent `chat.completion.chunk` events, the
                             usage chunk when `stream_options.include_usage` asks for it,
                             then `data: [DONE]`

Responses are a pure function of the request body (no clock, no randomness, no
network), so repeated calls are byte-identical. This is dev infrastructure, not
a model gateway: the real provider path is python/intelligence/model_gateway.
"""
from __future__ import annotations

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

STUB_MODEL = "local-stub-instruct"


def completion(body: dict[str, Any]) -> dict[str, Any]:
    messages = body.get("messages") or []
    last_user = ""
    for message in messages:
        if isinstance(message, dict) and message.get("role") == "user":
            content = message.get("content")
            last_user = content if isinstance(content, str) else json.dumps(content, sort_keys=True)
    prompt_chars = len(last_user)
    text = f"stub completion: {prompt_chars} prompt characters received"
    return {
        "id": "chatcmpl-stub-deterministic",
        "object": "chat.completion",
        "model": body.get("model") or STUB_MODEL,
        "choices": [
            {
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop",
            }
        ],
        "usage": {
            "prompt_tokens": max(1, prompt_chars // 4),
            "completion_tokens": len(text) // 4,
            "total_tokens": max(1, prompt_chars // 4) + len(text) // 4,
        },
    }


def completion_stream(body: dict[str, Any]) -> bytes:
    """The completion as the OpenAI streaming wire: role, content deltas, stop, usage, done."""
    whole = completion(body)
    text = whole["choices"][0]["message"]["content"]
    head, _, tail = text.partition(": ")
    frame = {"id": whole["id"], "object": "chat.completion.chunk", "model": whole["model"]}

    def chunk(delta: dict[str, str], finish_reason: str | None = None) -> dict[str, Any]:
        return {**frame, "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]}

    chunks = [
        chunk({"role": "assistant", "content": ""}),
        chunk({"content": f"{head}: "}),
        chunk({"content": tail}),
        chunk({}, "stop"),
    ]
    options = body.get("stream_options")
    if isinstance(options, dict) and options.get("include_usage"):
        chunks.append({**frame, "choices": [], "usage": whole["usage"]})
    events = [f"data: {json.dumps(chunk, sort_keys=True)}\n\n" for chunk in chunks]
    events.append("data: [DONE]\n\n")
    return "".join(events).encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, status: int, payload: dict[str, Any]) -> None:
        self._send_bytes(status, "application/json", json.dumps(payload, sort_keys=True).encode())

    def _send_bytes(self, status: int, content_type: str, data: bytes) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path == "/healthz":
            self._send(200, {"status": "ok"})
        elif self.path == "/v1/models":
            self._send(200, {"object": "list", "data": [{"id": STUB_MODEL, "object": "model"}]})
        else:
            self._send(404, {"error": {"message": "not found", "type": "invalid_request_error"}})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path != "/v1/chat/completions":
            self._send(404, {"error": {"message": "not found", "type": "invalid_request_error"}})
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            self._send(400, {"error": {"message": "invalid json", "type": "invalid_request_error"}})
            return
        if body.get("stream") is True:
            self._send_bytes(200, "text/event-stream", completion_stream(body))
            return
        self._send(200, completion(body))

    def log_message(self, format: str, *args: Any) -> None:
        print(f"stub-provider: {format % args}", flush=True)


def main() -> None:
    port = int(os.environ.get("QUANSIO_STUB_PROVIDER_PORT", "59020"))
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    print(f"stub-provider: listening on :{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
