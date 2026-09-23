"""Outbound webhook signature verification (APP-015, DOMAIN.md §13.5).

A delivery carries ``quansio-signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret,
"<t>.<body>")>``. Verify it over the raw request body, before parsing it, with the
subscription's ``whsec_…`` secret, and reject a timestamp outside the tolerance so a captured
delivery cannot be replayed later. Deliveries are at-least-once: dedupe on
``quansio-event-id``. Header names, scheme and tolerance are the generated contract
(``generated.operations``); this module is only the arithmetic, standard library only.
"""

from __future__ import annotations

import hashlib
import hmac
import time
from typing import Literal

from .generated.operations import (
    WEBHOOK_HEADERS,
    WEBHOOK_SECRET_PREFIX,
    WEBHOOK_SIGNATURE_HEADER,
    WEBHOOK_SIGNATURE_SCHEME,
    WEBHOOK_TOLERANCE_SECONDS,
)

__all__ = [
    "WEBHOOK_HEADERS",
    "WEBHOOK_SECRET_PREFIX",
    "WEBHOOK_SIGNATURE_HEADER",
    "WEBHOOK_TOLERANCE_SECONDS",
    "WebhookSignatureError",
    "parse_webhook_signature",
    "verify_webhook_signature",
]

Reason = Literal["malformed", "stale", "mismatch"]


class WebhookSignatureError(Exception):
    """A signature was refused; ``reason`` says why."""

    def __init__(self, reason: Reason) -> None:
        super().__init__(f"webhook signature {reason}")
        self.reason: Reason = reason


def parse_webhook_signature(header: str) -> tuple[int, list[str]] | None:
    """The ``t=`` timestamp and every signature for the contract's scheme in a header."""
    timestamp: int | None = None
    signatures: list[str] = []
    for part in header.split(","):
        key, sep, value = part.strip().partition("=")
        if not sep:
            continue
        if key == "t":
            try:
                timestamp = int(value)
            except ValueError:
                timestamp = None
        elif key == WEBHOOK_SIGNATURE_SCHEME:
            signatures.append(value)
    if timestamp is None or not signatures:
        return None
    return timestamp, signatures


def verify_webhook_signature(
    secret: str,
    header: str,
    body: str | bytes,
    *,
    now: int | None = None,
    tolerance_seconds: int = WEBHOOK_TOLERANCE_SECONDS,
) -> None:
    """Return when the signature is valid; raise :class:`WebhookSignatureError` otherwise."""
    parsed = parse_webhook_signature(header)
    if parsed is None:
        raise WebhookSignatureError("malformed")
    timestamp, signatures = parsed
    current = int(time.time()) if now is None else now
    if abs(current - timestamp) > tolerance_seconds:
        raise WebhookSignatureError("stale")
    raw = body.encode() if isinstance(body, str) else body
    expected = hmac.new(
        secret.encode(), f"{timestamp}.".encode() + raw, hashlib.sha256
    ).hexdigest()
    if any(
        hmac.compare_digest(expected, candidate.lower()) for candidate in signatures
    ):
        return
    raise WebhookSignatureError("mismatch")
