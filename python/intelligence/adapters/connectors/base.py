"""Connector adapter framework (EXEC-011, DOMAIN.md §7.5 `connector.<id>.<op>`).

An adapter is a thin, typed surface over one provider: it declares its
operations, builds provider requests, and parses provider responses. Two rules
are structural:

* **Token isolation.** An adapter never receives or returns token material. It
  is given the credential *handle* (`sec_…`) and passes it opaquely to the
  transport, which resolves handles at dispatch through the secret-store owner.
* **Effects are not here.** Consequential writes reserve/settle EffectRecords
  in the Rust runtime; an adapter only builds and parses. The conformance
  suite (`conformance.py`) is the shared gate every adapter must pass.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Protocol

HANDLE_PREFIX = "sec_"


class AdapterError(ValueError):
    """A refused adapter call, naming the rule that refused it."""

    def __init__(self, code: str, detail: str) -> None:
        super().__init__(f"{code}: {detail}")
        self.code = code
        self.detail = detail


def require_handle(handle: str) -> str:
    """Accept only a `sec_` handle; raw token material is refused at the seam."""
    if not handle.startswith(HANDLE_PREFIX):
        raise AdapterError("VALIDATION_SCHEMA", "credentials must be sec_ handles, never raw tokens")
    return handle


class HttpTransport(Protocol):
    """The outbound port. Production resolves handles to tokens at dispatch."""

    def request(
        self,
        *,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> tuple[int, bytes]: ...


@dataclass(frozen=True, slots=True)
class Operation:
    """One declared `connector.<id>.<op>` surface."""

    name: str
    method: str
    path: str
    consequential: bool
    required: tuple[str, ...] = ()


@dataclass(slots=True)
class ConnectorAdapter:
    """Base for the GA adapters: id, ops, and dispatch over a transport."""

    connector_id: str
    base_url: str
    operations: tuple[Operation, ...]
    authorize_url: str = ""
    scope: str = ""
    extra_headers: Mapping[str, str] = field(default_factory=dict)

    def operation(self, name: str) -> Operation:
        for op in self.operations:
            if op.name == name:
                return op
        raise AdapterError("VALIDATION_SCHEMA", f"unknown operation {name!r} for {self.connector_id}")

    def authorize_redirect(self, state: str) -> str:
        """The provider redirect for `BeginConnectorAuth` (state comes from the broker)."""
        if not self.authorize_url:
            raise AdapterError("VALIDATION_SCHEMA", f"{self.connector_id} declares no authorize URL")
        return f"{self.authorize_url}?state={state}&scope={self.scope}"

    def build_request(
        self,
        op_name: str,
        args: Mapping[str, str],
        credential_handle: str,
    ) -> tuple[Operation, str, str, Mapping[str, str], bytes]:
        """Build the provider request, enforcing the closed op vocabulary and handle rule."""
        op = self.operation(op_name)
        require_handle(credential_handle)
        missing = [key for key in op.required if key not in args]
        if missing:
            raise AdapterError("VALIDATION_SCHEMA", f"{op_name}: missing arguments {missing}")
        path = op.path.format(**{key: str(args[key]) for key in op.required})
        headers = {
            **self.extra_headers,
            "X-Quansio-Credential-Handle": credential_handle,
            "Accept": "application/json",
        }
        body = b"" if op.method == "GET" else str(dict(args)).encode("utf-8")
        return op, op.method, f"{self.base_url}{path}", headers, body

    def invoke(
        self,
        transport: HttpTransport,
        op_name: str,
        args: Mapping[str, str],
        credential_handle: str,
    ) -> Mapping[str, object]:
        """Run one operation through the transport; the handle travels opaquely."""
        op, method, url, headers, body = self.build_request(op_name, args, credential_handle)
        status, payload = transport.request(method=method, url=url, headers=headers, body=body)
        if status >= 400:
            raise AdapterError(
                "PROVIDER_UNAVAILABLE", f"{self.connector_id}.{op_name}: provider returned {status}"
            )
        import json

        return {
            "op": f"{self.connector_id}.{op.name}",
            "consequential": op.consequential,
            **json.loads(payload),
        }
