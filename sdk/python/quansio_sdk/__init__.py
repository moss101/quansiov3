"""Quansio public API v1 client for Python (APP-013).

Canonical owner: ``sdk/python``. The operation table (``OPERATIONS``, ``READ_PROJECTIONS``,
``ERROR_CODES``) is generated from the DOMAIN.md catalog by GOV-004's ``gen_contracts.py``;
this module is the transport, standard library only: a bearer session (or a development
harness's trusted headers), ``POST /v1/commands/<Name>`` with a client-minted command id,
``GET /v1/...`` projections, and the §15 error body as a typed :class:`ApiError`. The client
addresses operations by id and refuses one the catalog does not have before any request.
"""

from __future__ import annotations

import json
import secrets
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from typing import Any

from .generated.operations import ERROR_CODES, OPERATIONS, READ_PROJECTIONS
from .webhooks import (
    WebhookSignatureError,
    parse_webhook_signature,
    verify_webhook_signature,
)

__all__ = [
    "ERROR_CODES",
    "OPERATIONS",
    "PUBLIC_API_VERSION",
    "READ_PROJECTIONS",
    "ApiError",
    "Client",
    "Credential",
    "WebhookSignatureError",
    "new_command_id",
    "parse_webhook_signature",
    "public_api_path",
    "verify_webhook_signature",
]

PUBLIC_API_VERSION = "v1"
_PREFIX = f"/{PUBLIC_API_VERSION}/"
_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"


def new_command_id() -> str:
    """A client-minted, ULID-shaped command id (the server's idempotency key)."""
    millis = int(time.time() * 1000)
    stamp = ""
    for _ in range(10):
        stamp = _ALPHABET[millis % 32] + stamp
        millis //= 32
    entropy = "".join(_ALPHABET[b % 32] for b in secrets.token_bytes(16))
    return f"cmd_{stamp}{entropy}"


def public_api_path(resource: str) -> str:
    """``/v1/<resource>`` with the resource validated; nothing else is addressed."""
    if (
        not resource
        or resource.startswith("/")
        or ".." in resource
        or "://" in resource
        or any(c.isspace() for c in resource)
    ):
        raise ValueError(f"invalid public API resource: {resource!r}")
    return f"{_PREFIX}{resource}"


class ApiError(Exception):
    """The §15 error body, typed."""

    def __init__(self, status: int, body: Mapping[str, Any]) -> None:
        self.status = status
        self.code = str(body.get("code", "INTERNAL"))
        self.message = str(body.get("message", f"HTTP {status}"))
        self.correlation_id = str(body.get("correlation_id", ""))
        self.retryable = bool(body.get("retryable", False))
        self.details = body.get("details")
        super().__init__(f"{self.code}: {self.message}")

    @property
    def is_catalogued(self) -> bool:
        """Whether the code is in the catalogued taxonomy."""
        return self.code in ERROR_CODES


@dataclass(frozen=True)
class Credential:
    """A bearer session, or (development harnesses only) a trusted-header scope."""

    access_token: Callable[[], str | None] | None = None
    tenant_id: str | None = None
    user_id: str | None = None

    def headers(self) -> dict[str, str]:
        if self.access_token is not None:
            token = self.access_token()
            return {"Authorization": f"Bearer {token}"} if token else {}
        if self.tenant_id and self.user_id:
            return {"x-quansio-tenant": self.tenant_id, "x-quansio-user": self.user_id}
        return {}


Transport = Callable[[str, str, dict[str, str], bytes | None], tuple[int, bytes]]


def _urllib_transport(
    method: str, url: str, headers: dict[str, str], body: bytes | None
) -> tuple[int, bytes]:
    request = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        # The caller chose the origin; the SDK addresses only /v1/ paths under it.
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


@dataclass
class Client:
    """A client of one server origin with one credential."""

    origin: str
    credential: Credential = field(default_factory=Credential)
    transport: Transport = _urllib_transport

    def __post_init__(self) -> None:
        self.origin = self.origin.rstrip("/")

    def get(self, resource: str) -> Any:
        """``GET /v1/<resource>`` (a read projection)."""
        return self._request("GET", public_api_path(resource))

    def command(
        self, operation: str, params: Mapping[str, Any], command_id: str | None = None
    ) -> Any:
        """``POST /v1/commands/<Name>`` for a catalogued operation; the owner's result."""
        return self.command_envelope(operation, params, command_id)["result"]

    def command_envelope(
        self, operation: str, params: Mapping[str, Any], command_id: str | None = None
    ) -> dict[str, Any]:
        """The whole envelope (``command_id``, ``replayed``, ``result``)."""
        entry = OPERATIONS.get(operation)
        if entry is None:
            raise ValueError(f"{operation!r} is not a catalogued operation")
        body = json.dumps(
            {"command_id": command_id or new_command_id(), "params": dict(params)}
        ).encode("utf-8")
        response = self._request(entry["method"], entry["path"], body)
        if not isinstance(response, dict):
            raise ApiError(
                500, {"code": "INTERNAL", "message": "the command returned no envelope"}
            )
        return response

    def health(self) -> dict[str, Any]:
        """``GET /v1/health``."""
        return dict(self.get("health"))

    def me(self) -> dict[str, Any]:
        """``GET /v1/me``."""
        return dict(self.get("me"))

    def start_magic_link(self, email: str, display_name: str = "") -> Any:
        """``POST /v1/auth/magic-link/start`` — no session yet."""
        return self._request(
            "POST",
            f"{_PREFIX}auth/magic-link/start",
            json.dumps({"email": email, "display_name": display_name}).encode(),
            authenticated=False,
        )

    def complete_magic_link(self, token: str) -> dict[str, Any]:
        """``POST /v1/auth/magic-link/complete`` — the issued session."""
        return dict(
            self._request(
                "POST",
                f"{_PREFIX}auth/magic-link/complete",
                json.dumps({"token": token}).encode(),
                authenticated=False,
            )
        )

    def _request(
        self,
        method: str,
        path: str,
        body: bytes | None = None,
        authenticated: bool = True,
    ) -> Any:
        if not path.startswith(_PREFIX):
            raise ValueError(f"not a public API path: {path}")
        headers = {"Accept": "application/json", "x-quansio-client": "sdk-python"}
        if authenticated:
            headers.update(self.credential.headers())
        if body is not None:
            headers["Content-Type"] = "application/json"
        status, raw = self.transport(method, f"{self.origin}{path}", headers, body)
        decoded: Any = None
        if raw:
            try:
                decoded = json.loads(raw)
            except ValueError:
                decoded = None
        if not 200 <= status < 300:
            raise ApiError(
                status,
                decoded
                if isinstance(decoded, dict)
                else {"message": raw.decode("utf-8", "replace")},
            )
        return decoded
