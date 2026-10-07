"""ADP-003 redaction test, the plane's copy of rule set ``v1`` (DOMAIN.md §11.7 C).

The hand-written corpus at ``tests/contract/fixtures/recording/redaction_corpus.json`` is the contract the
recorder, the server and the plane share; this file proves the plane's implementation agrees with every
case, that redaction is idempotent, that summaries carry counts and never values, and that the compiler's
gate refuses a projection in which redaction would still remove something.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from intelligence.skills.recording import (
    CATEGORIES,
    RULE_SET,
    RecordingCompileError,
    assert_redacted,
    redact_field_text,
    redact_text,
    scan_text,
)

CORPUS = json.loads(
    (
        Path(__file__).resolve().parents[3] / "tests/contract/fixtures/recording/redaction_corpus.json"
    ).read_text()
)
CASES = CORPUS["cases"]


def test_the_corpus_is_the_contract() -> None:
    assert CORPUS["rule_set"] == RULE_SET
    assert tuple(CORPUS["categories"]) == CATEGORIES
    assert len(CASES) >= 40
    assert len({case["id"] for case in CASES}) == len(CASES)


@pytest.mark.parametrize("case", CASES, ids=[case["id"] for case in CASES])
def test_the_plane_agrees_with_the_corpus(case: dict[str, object]) -> None:
    text = str(case["input"])
    if "field" in case:
        redacted, counts = redact_field_text(case["field"], text)  # type: ignore[arg-type]
    else:
        redacted, counts = redact_text(text)
    assert redacted == case["redacted"], case["id"]
    assert counts == case["counts"], case["id"]
    if redacted is not None:
        # Idempotent, and a scan of the output finds nothing: the property the server's gate relies on.
        assert redact_text(redacted)[0] == redacted
        assert scan_text(redacted) == {}


def test_a_summary_is_counts_and_never_a_value() -> None:
    text, counts = redact_text(
        "AKIAIOSFODNN7EXAMPLE and card 4111 1111 1111 1111 and Bearer abcdefgh12345678"
    )
    assert counts == {"api_key": 1, "card_number": 1, "bearer_token": 1}
    assert "AKIA" not in text and "4111" not in text and "abcdefgh" not in text
    assert all(isinstance(n, int) for n in counts.values())


def test_the_compiler_refuses_a_projection_that_still_holds_a_secret_and_never_names_it() -> None:
    projection = {
        "redaction": {"rule_set": "v1", "counts": {}},
        "actions": [{"seq": 1, "text": "key AKIAIOSFODNN7EXAMPLE"}],
        "transcript": [{"at": "t", "text": "my card is 4111111111111111"}],
    }
    with pytest.raises(RecordingCompileError) as refused:
        assert_redacted(projection)
    assert refused.value.code == "RECORDING_UNREDACTED"
    rendered = str(refused.value)
    assert "api_key" in rendered and "card_number" in rendered
    assert "AKIA" not in rendered and "4111" not in rendered
    assert "/actions/0/text" in rendered


def test_a_redacted_projection_passes_the_gate() -> None:
    assert_redacted({"actions": [{"text": "[REDACTED:api_key] then [REDACTED:card_number]"}]})
