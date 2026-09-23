"""APP-015: the Python SDK verifies a webhook exactly as the server signs it.

The vector is the one the server's own test asserts
(``crates/server/src/notify/webhooks/signature.rs``), so the two sides cannot drift apart. Pure:
no server.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "sdk" / "python"))

from quansio_sdk import (
    WebhookSignatureError,
    parse_webhook_signature,
    verify_webhook_signature,
)
from quansio_sdk.webhooks import (
    WEBHOOK_HEADERS,
    WEBHOOK_SECRET_PREFIX,
    WEBHOOK_SIGNATURE_HEADER,
    WEBHOOK_TOLERANCE_SECONDS,
)

SECRET = "whsec_dGVzdC1vbmx5LXNpZ25pbmctc2VjcmV0LTMyYnl0ZXMh"
BODY = '{"event_id":"evt_01J8Z3K6F1N8VQ2X5W9Y0ABCDE","type":"run.succeeded"}'
TIMESTAMP = 1_790_000_000
HEADER = (
    "t=1790000000,v1=9852831d03e3f0aa4d104a985bb8b53967170389ff827d1d3a6492fe2d2b4adb"
)


def reason(**kwargs: object) -> str:
    try:
        verify_webhook_signature(**kwargs)  # type: ignore[arg-type]
    except WebhookSignatureError as error:
        return error.reason
    return "ok"


def test_the_contract_is_generated_from_domain() -> None:
    assert WEBHOOK_SIGNATURE_HEADER == "quansio-signature"
    assert "quansio-event-id" in WEBHOOK_HEADERS
    assert WEBHOOK_TOLERANCE_SECONDS == 300
    assert SECRET.startswith(WEBHOOK_SECRET_PREFIX)


def test_the_servers_signature_verifies_over_the_raw_body() -> None:
    assert reason(secret=SECRET, header=HEADER, body=BODY, now=TIMESTAMP) == "ok"
    assert (
        reason(secret=SECRET, header=HEADER, body=BODY.encode(), now=TIMESTAMP + 299)
        == "ok"
    )


@pytest.mark.parametrize(
    ("kwargs", "expected"),
    [
        (
            {"secret": "whsec_other", "header": HEADER, "body": BODY, "now": TIMESTAMP},
            "mismatch",
        ),
        (
            {"secret": SECRET, "header": HEADER, "body": BODY + " ", "now": TIMESTAMP},
            "mismatch",
        ),
        (
            {"secret": SECRET, "header": HEADER, "body": BODY, "now": TIMESTAMP + 301},
            "stale",
        ),
        (
            {"secret": SECRET, "header": "v1=00", "body": BODY, "now": TIMESTAMP},
            "malformed",
        ),
        (
            {"secret": SECRET, "header": "t=x,v1=00", "body": BODY, "now": TIMESTAMP},
            "malformed",
        ),
    ],
)
def test_anything_else_is_refused_with_a_reason(
    kwargs: dict[str, object], expected: str
) -> None:
    assert reason(**kwargs) == expected


def test_a_rotation_window_verifies_with_either_signature() -> None:
    rotated = f"{HEADER},v1={'0' * 64}"
    parsed = parse_webhook_signature(rotated)
    assert parsed is not None and len(parsed[1]) == 2
    assert reason(secret=SECRET, header=rotated, body=BODY, now=TIMESTAMP) == "ok"
