"""The shared connector conformance suite (EXEC-011 acceptance 3).

Every GA adapter must pass this suite. Two tiers:

* **contract tier (always runs).** Operation vocabulary is closed, required
  arguments are enforced, handles are the only credentials accepted, and no
  output ever carries token material. This runs offline against a transport the
  caller supplies.
* **sandbox tier (gated).** The provider-sandbox run executes only when the
  adapter's `QUANSIO_TEST_CONNECTOR_<NAME>=1` flag is set. When the flag is
  absent the tier reports an explicit `BLOCKED_EXTERNAL` marker — it is never
  silently skipped and never substituted with a mock.
"""

from __future__ import annotations

import os
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Protocol

from .base import AdapterError, ConnectorAdapter

CASE_HANDLE = "sec_01J8Z3K6F1N8VQ2X5W9Y0CONFORM"
RAW_TOKEN = "gho_rawTokenMaterialMustNeverAppear"


class ConformanceTransport(Protocol):
    """What the suite drives adapters through."""

    def request(
        self, *, method: str, url: str, headers: Mapping[str, str], body: bytes | None
    ) -> tuple[int, bytes]: ...


@dataclass(frozen=True, slots=True)
class TierResult:
    """One tier's outcome for one adapter."""

    adapter_id: str
    tier: str
    passed: bool
    detail: str


def _contract_cases(adapter: ConnectorAdapter) -> list[tuple[str, Mapping[str, str] | None, bool]]:
    """(op, args, should_pass) rows: the happy path plus the refusals."""
    cases: list[tuple[str, Mapping[str, str] | None, bool]] = []
    for op in adapter.operations:
        args = dict.fromkeys(op.required, "x")
        cases.append((op.name, args, True))
        if op.required:
            cases.append((op.name, None, False))
    cases.append(("not.an.operation", None, False))
    return cases


class RecordingTransport:
    """Records requests and answers `{"ok": true}` so parsing is exercised too."""

    def __init__(self) -> None:
        self.seen: list[tuple[str, str, Mapping[str, str]]] = []

    def request(
        self, *, method: str, url: str, headers: Mapping[str, str], body: bytes | None
    ) -> tuple[int, bytes]:
        self.seen.append((method, url, dict(headers)))
        return 200, b'{"ok": true}'


def run_contract_tier(adapter: ConnectorAdapter) -> TierResult:
    """The offline tier: vocabulary, argument enforcement, handle-only credentials."""
    transport = RecordingTransport()
    try:
        for op_name, args, should_pass in _contract_cases(adapter):
            try:
                result = adapter.invoke(transport, op_name, args or {}, CASE_HANDLE)
            except AdapterError:
                if should_pass:
                    return TierResult(
                        adapter.connector_id, "contract", False, f"{op_name}: refused a valid call"
                    )
                continue
            if not should_pass:
                return TierResult(
                    adapter.connector_id, "contract", False, f"{op_name}: accepted an invalid call"
                )
            flat = str(result) + str(transport.seen[-1])
            if RAW_TOKEN in flat:
                return TierResult(
                    adapter.connector_id, "contract", False, "token material leaked through the adapter"
                )
        # Raw material is refused at the seam.
        try:
            adapter.invoke(transport, adapter.operations[0].name, {}, RAW_TOKEN)
        except AdapterError:
            return TierResult(adapter.connector_id, "contract", True, "offline contract tier passed")
        return TierResult(adapter.connector_id, "contract", False, "raw token material was accepted")
    except AdapterError as error:
        return TierResult(adapter.connector_id, "contract", False, f"unexpected refusal: {error}")


def sandbox_flag(adapter: ConnectorAdapter) -> str:
    """The env flag that gates this adapter's provider-sandbox run."""
    return "QUANSIO_TEST_CONNECTOR_" + adapter.connector_id.upper().replace("-", "_")


def run_sandbox_tier(adapter: ConnectorAdapter, transport: ConformanceTransport) -> TierResult:
    """The provider-sandbox tier, gated on the adapter's flag."""
    if os.environ.get(sandbox_flag(adapter), "") != "1":
        return TierResult(
            adapter.connector_id,
            "sandbox",
            False,
            f"BLOCKED_EXTERNAL: {sandbox_flag(adapter)}=1 is not set; provider sandbox not running",
        )
    try:
        for op in adapter.operations:
            adapter.invoke(
                transport,
                op.name,
                dict.fromkeys(op.required, "sandbox"),
                CASE_HANDLE,
            )
    except AdapterError as error:
        return TierResult(adapter.connector_id, "sandbox", False, f"sandbox run failed: {error}")
    return TierResult(adapter.connector_id, "sandbox", True, "sandbox tier passed")


def run_suite(
    adapters: Sequence[ConnectorAdapter],
    sandbox_transport: ConformanceTransport,
) -> list[TierResult]:
    """Run both tiers for every adapter, in adapter order."""
    results: list[TierResult] = []
    for adapter in adapters:
        results.append(run_contract_tier(adapter))
        results.append(run_sandbox_tier(adapter, sandbox_transport))
    return results
