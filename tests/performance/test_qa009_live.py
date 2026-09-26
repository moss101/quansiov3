"""QA-009 with a model behind the gateway: the first token, and usage accuracy.

The served product runs with the Python intelligence service routed to one provider; the same provider
is also called directly, alternately with the Quansio calls, as the baseline:

* against the dev stack's stub provider, which answers at once, the difference is Quansio's own
  overhead on the first token — what DOSSIER §21.2 bounds (≤ +300 ms);
* against a live provider, the difference also carries what Quansio sends (its instructions, skills
  and tool schemas: a reasoning model reasons longer over them before answering), so it is recorded,
  and the live run asserts what is Quansio's alone: text streams, and usage is metered exactly.

Environment: as `test_qa009_campaign`; `QUANSIO_TEST_STUB_PROVIDER_URL` (default the dev stack's);
`QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL`, `_API_KEY` and `_MODEL_ID` for the live run. Absent →
`BLOCKED_EXTERNAL`.
"""

from __future__ import annotations

import json
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
import yaml

import qa009_harness as harness
from qa009_harness import (
    UNLIMITED,
    WORKSPACE,
    WebSocket,
    meter,
    percentile,
    record,
    summary,
)

SLOS: dict[str, Any] = yaml.safe_load(
    (Path(__file__).parent / "slos.yaml").read_text()
)["slos"]

PROMPT = "In one short sentence, say what a release checklist is for."
SAMPLES = 12


def _post_in_background(
    server: harness.ServerUnderLoad, thread: str, content: str
) -> tuple[threading.Thread, dict[str, Any]]:
    """Post a message on its own connection."""
    poster = server.client()
    outcome: dict[str, Any] = {}

    def post() -> None:
        outcome["result"] = poster.command(
            "PostMessage",
            {"workspace_id": WORKSPACE, "thread_id": thread, "content": content},
        )

    sender = threading.Thread(target=post)
    sender.start()
    return sender, outcome


def _first_tokens(
    server: harness.ServerUnderLoad, baseline: Callable[[], float]
) -> tuple[list[float], list[float], int]:
    """Alternate a direct call and a message through Quansio; time each one's first text."""
    owner = server.client()
    thread = owner.command(
        "CreateThread", {"workspace_id": WORKSPACE, "title": "first token"}
    )["result"]["thread_id"]
    stream = WebSocket(
        server.origin,
        server.token,
        {"subscribe": [{"channel": f"workspace:{WORKSPACE}"}]},
    )
    try:
        while stream.recv(0.5) is not None:
            pass
        direct: list[float] = []
        through: list[float] = []
        streamed_first = 0
        for index in range(SAMPLES):
            direct.append(baseline())
            started = time.perf_counter()
            sender, outcome = _post_in_background(server, thread, f"{PROMPT} ({index})")
            first: float | None = None
            while True:
                message = stream.recv(120)
                assert message is not None, "no model frame within 120 s"
                frame = json.loads(message)
                live = frame.get("frame") or {}
                if frame.get("kind") != "live" or live.get("kind") != "model.delta":
                    continue
                if first is None and live.get("text"):
                    first = (time.perf_counter() - started) * 1000
                    streamed_first += live.get("final") is False
                if live.get("final") is True:
                    break
            sender.join(timeout=120)
            assert "result" in outcome, "the message was not accepted"
            assert first is not None
            through.append(first)
        return direct, through, streamed_first
    finally:
        stream.close()


# BLOCKED_EXTERNAL without QUANSIO_TEST_PERF=1, QUANSIO_TEST_POSTGRES_URL, the server builds and the stub provider
# (QUANSIO_TEST_STUB_PROVIDER_URL).
@pytest.mark.skipif(
    harness.stub_blocked_reason() is not None,
    reason=harness.stub_blocked_reason() or "",
)
def test_quansio_adds_little_to_a_models_first_token() -> None:
    stub = f"{harness.STUB_PROVIDER_URL}/v1"

    def baseline() -> float:
        return harness.provider_first_token_ms(
            PROMPT, 4096, stub, "qa009-stub", "stub-credential"
        )

    with (
        harness.gateway(stub, "qa009-stub", "stub-credential") as endpoint,
        harness.start(
            "firsttoken", {**UNLIMITED, "QUANSIO_INTELLIGENCE_ENDPOINT": endpoint}
        ) as server,
    ):
        direct, through, streamed_first = _first_tokens(server, baseline)
    result = record(
        "model_stream_first_token_over_baseline_ms",
        {
            "provider": "the dev stack's stub provider (answers at once: the difference is Quansio's)",
            "provider_first_token": summary(direct),
            "through_quansio_first_token": summary(through),
            "first_frame_was_a_partial_delta": f"{streamed_first}/{SAMPLES}",
            "overhead_p50_ms": round(
                percentile(through, 0.5) - percentile(direct, 0.5), 1
            ),
            "overhead_p95_ms": round(
                percentile(through, 0.95) - percentile(direct, 0.95), 1
            ),
            "target_ms": SLOS["model_stream_first_token_over_baseline_ms"]["target"],
        },
    )
    assert streamed_first == SAMPLES, result
    assert result["overhead_p95_ms"] <= result["target_ms"], result


# BLOCKED_EXTERNAL without the stack and a live provider (QUANSIO_TEST_OPENAI_COMPATIBLE_*).
@pytest.mark.skipif(
    harness.live_blocked_reason() is not None,
    reason=harness.live_blocked_reason() or "",
)
def test_a_live_model_streams_and_its_usage_is_metered_exactly() -> None:
    with (
        harness.gateway() as endpoint,
        harness.start(
            "live", {**UNLIMITED, "QUANSIO_INTELLIGENCE_ENDPOINT": endpoint}
        ) as server,
    ):
        direct, through, streamed_first = _first_tokens(
            server, lambda: harness.provider_first_token_ms(PROMPT)
        )
        calls = server.sql(
            "SELECT COALESCE(SUM((usage->>'input')::bigint), 0), "
            "COALESCE(SUM((usage->>'output')::bigint), 0), count(*) FROM model_calls"
        )[0]
        usage = server.client().get("/v1/usage", workspace_id=WORKSPACE)
    first_token = record(
        "model_first_token_live_provider_ms",
        {
            "provider": "OpenAI-compatible (QUANSIO_TEST_OPENAI_COMPATIBLE_*)",
            "model": harness.LIVE_MODEL_ID,
            "provider_first_token_bare_prompt": summary(direct),
            "through_quansio_first_token": summary(through),
            "first_frame_was_a_partial_delta": f"{streamed_first}/{SAMPLES}",
            "difference_p50_ms": round(
                percentile(through, 0.5) - percentile(direct, 0.5), 1
            ),
            "note": (
                "the direct call sends the bare prompt; through Quansio the model also receives the "
                "teammate's instructions, skills and tool schemas and reasons over them first"
            ),
        },
    )
    usage_result = record(
        "usage_accuracy_model_tokens",
        {
            "model_calls": int(calls[2]),
            "ledger_input_tokens": int(calls[0]),
            "ledger_output_tokens": int(calls[1]),
            "metered_input_tokens": meter(usage, "model_input_tokens"),
            "metered_output_tokens": meter(usage, "model_output_tokens"),
        },
    )
    # What is Quansio's alone: the reply streams to the person, and the tokens the provider reported
    # are exactly what the tenant is metered.
    assert streamed_first == SAMPLES, first_token
    assert (
        usage_result["ledger_input_tokens"] > 0
        and usage_result["ledger_output_tokens"] > 0
    )
    assert usage_result["metered_input_tokens"] == float(
        usage_result["ledger_input_tokens"]
    )
    assert usage_result["metered_output_tokens"] == float(
        usage_result["ledger_output_tokens"]
    )
