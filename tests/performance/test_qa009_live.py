"""QA-009 with a live model: first token over the provider's own baseline, and usage accuracy.

The served product runs with the Python intelligence service routed to a live OpenAI-compatible
provider. The baseline is the same provider called directly with the same prompt, alternated with
the Quansio calls so both see the same provider weather.

Environment: as `test_qa009_campaign`, plus `QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL`,
`QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY` and `QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID`. Absent →
`BLOCKED_EXTERNAL`.
"""

from __future__ import annotations

import json
import threading
import time
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
BLOCKED = harness.live_blocked_reason()
pytestmark = pytest.mark.skipif(BLOCKED is not None, reason=BLOCKED or "")

PROMPT = "In one short sentence, say what a release checklist is for."
SAMPLES = 12


def _post_in_background(
    server: harness.ServerUnderLoad, thread: str, content: str
) -> tuple[threading.Thread, dict[str, Any]]:
    """Post a message on its own connection; `PostMessage` answers only when the turn is done."""
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


def test_the_first_model_token_reaches_a_person_within_the_budget_over_the_provider() -> (
    None
):
    with (
        harness.gateway() as endpoint,
        harness.start(
            "live", {**UNLIMITED, "QUANSIO_INTELLIGENCE_ENDPOINT": endpoint}
        ) as server,
    ):
        owner = server.client()
        thread = owner.command(
            "CreateThread", {"workspace_id": WORKSPACE, "title": "live"}
        )["result"]["thread_id"]
        stream = WebSocket(
            server.origin,
            server.token,
            {"subscribe": [{"channel": f"workspace:{WORKSPACE}"}]},
        )
        while stream.recv(0.5) is not None:
            pass
        baseline: list[float] = []
        through: list[float] = []
        streamed_first = 0
        for index in range(SAMPLES):
            baseline.append(harness.provider_first_token_ms(PROMPT))
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
        stream.close()

        calls = server.sql(
            "SELECT COALESCE(SUM((usage->>'input')::bigint), 0), COALESCE(SUM((usage->>'output')::bigint), 0), "
            "count(*) FROM model_calls"
        )[0]
        usage = owner.get("/v1/usage", workspace_id=WORKSPACE)
        overhead_p50 = percentile(through, 0.5) - percentile(baseline, 0.5)
        overhead_p95 = percentile(through, 0.95) - percentile(baseline, 0.95)
        result = record(
            "model_stream_first_token_over_baseline_ms",
            {
                "provider": "OpenAI-compatible (QUANSIO_TEST_OPENAI_COMPATIBLE_*)",
                "model": harness.LIVE_MODEL_ID,
                "provider_first_token": summary(baseline),
                "through_quansio_first_token": summary(through),
                "first_frame_was_a_partial_delta": f"{streamed_first}/{SAMPLES}",
                "overhead_p50_ms": round(overhead_p50, 1),
                "overhead_p95_ms": round(overhead_p95, 1),
                "target_ms": SLOS["model_stream_first_token_over_baseline_ms"][
                    "target"
                ],
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
        # Usage: what the tenant is metered is exactly what the providers reported per call.
        assert (
            usage_result["ledger_input_tokens"] > 0
            and usage_result["ledger_output_tokens"] > 0
        )
        assert usage_result["metered_input_tokens"] == float(
            usage_result["ledger_input_tokens"]
        ), usage_result
        assert usage_result["metered_output_tokens"] == float(
            usage_result["ledger_output_tokens"]
        ), usage_result
        # First token: text streams to the person, within the budget over the provider's own.
        assert streamed_first == SAMPLES, result
        assert result["overhead_p95_ms"] <= result["target_ms"], result
