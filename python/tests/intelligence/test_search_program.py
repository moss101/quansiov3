"""INT-005 typed SearchProgram: the vocabulary is closed and values are literals.

The AST-validation suite drives the shipped `SearchProgram.parse` with real payloads, including the
adversarial ones a predicate-string API would have accepted. A structural check also asserts the
module cannot evaluate anything: it imports no evaluator, so a value can never become code.
"""

from __future__ import annotations

import json
import pathlib

import pytest

from intelligence.context.search import (
    CHANNELS,
    OPERATORS,
    SearchProgram,
    SearchProgramError,
)


def test_every_channel_is_accepted_and_validated() -> None:
    """Each of the six channels parses, and the result is canonical."""
    for channel in CHANNELS:
        key = {
            "exact": "value",
            "lexical": "text",
            "semantic": "text",
            "graph": "node_id",
            "history": "run_id",
            "memory": "scope",
        }[channel]
        program = SearchProgram.parse(
            {"channels": [{"channel": channel, key: f"literal-{channel}"}], "limit": 5}
        )
        assert program.limit == 5
        assert program.channels[0].channel.value == channel
        assert json.loads(program.canonical_json())["channels"][0]["value"] == f"literal-{channel}"


def test_canonical_form_is_order_independent() -> None:
    """Two programs describing the same search produce identical bytes."""
    first = SearchProgram.parse(
        {
            "channels": [
                {"channel": "lexical", "text": "alpha"},
                {"channel": "exact", "value": "literal-1"},
            ],
            "predicates": [
                {"field": "kind", "operator": "eq", "value": "task"},
                {"field": "status", "operator": "in", "value": ["ready", "running"]},
            ],
        }
    )
    second = SearchProgram.parse(
        {
            "channels": [
                {"channel": "exact", "value": "literal-1"},
                {"channel": "lexical", "text": "alpha"},
            ],
            "predicates": [
                {"field": "status", "operator": "in", "value": ["ready", "running"]},
                {"field": "kind", "operator": "eq", "value": "task"},
            ],
        }
    )
    assert first.canonical_json() == second.canonical_json(), "the canonical form is a pure function"


@pytest.mark.parametrize(
    "value",
    [
        "' OR 1=1 --",
        "a) OR (1=1",
        "'; DROP TABLE artifacts; --",
        "$(whoami)",
        "${HOME}",
        "a && b",
        "title:foo/*",
        "`id`",
        "SELECT * FROM runs",
    ],
)
def test_an_expression_shaped_value_is_refused(value: str) -> None:
    """Acceptance 1: no executable or arbitrary predicate string is accepted."""
    with pytest.raises(SearchProgramError) as raised:
        SearchProgram.parse(
            {
                "channels": [{"channel": "exact", "value": "ok"}],
                "predicates": [{"field": "title", "operator": "contains", "value": value}],
            }
        )
    assert raised.value.code == "VALIDATION_SCHEMA"
    assert raised.value.rule_id == "program.literal_only"


def test_the_vocabulary_is_closed_everywhere() -> None:
    """An unknown channel, key, field, operator or value shape is refused, never ignored."""
    cases = [
        ({"channels": [{"channel": "telepathy", "value": "x"}]}, "program.channel"),
        ({"channels": [{"channel": "exact", "value": "x", "sql": "1=1"}]}, "program.term_shape"),
        ({"channels": [{"channel": "exact", "value": "x"}], "surprise": 1}, "program.known_keys"),
        (
            {
                "channels": [{"channel": "exact", "value": "x"}],
                "predicates": [{"field": "secret", "operator": "eq", "value": 1}],
            },
            "program.field",
        ),
        (
            {
                "channels": [{"channel": "exact", "value": "x"}],
                "predicates": [{"field": "title", "operator": "gt", "value": "a"}],
            },
            "program.operator",
        ),
        (
            {
                "channels": [{"channel": "exact", "value": "x"}],
                "predicates": [{"field": "status", "operator": "in", "value": []}],
            },
            "program.value",
        ),
        (
            {
                "channels": [{"channel": "exact", "value": "x"}],
                "predicates": [{"field": "agent_thread_id", "operator": "exists", "value": "yes"}],
            },
            "program.value",
        ),
        ({"channels": []}, "program.channel"),
        ({"channels": [{"channel": "exact", "value": "x"}], "limit": 0}, "program.limit"),
    ]
    for payload, rule in cases:
        with pytest.raises(SearchProgramError) as raised:
            SearchProgram.parse(payload)
        assert raised.value.rule_id == rule, f"{payload} should fail with {rule}"
        assert raised.value.code == "VALIDATION_SCHEMA"


def test_a_literal_may_contain_punctuation() -> None:
    """A value is never interpolated, so punctuation inside it is just text.

    This is the other half of acceptance 1: the protection is that values are literals in a closed
    vocabulary, not that they avoid punctuation — a file named `a=b` must remain searchable.
    """
    program = SearchProgram.parse(
        {
            "channels": [{"channel": "exact", "value": "a=b"}],
            "predicates": [{"field": "title", "operator": "contains", "value": "1=1"}],
        }
    )
    assert program.predicates[0].value == "1=1"


def test_operators_are_the_declared_set() -> None:
    """The operator vocabulary is the module's, and every declared operator is reachable."""
    assert "eq" in OPERATORS
    program = SearchProgram.parse(
        {
            "channels": [{"channel": "history", "run_id": "run_1"}],
            "predicates": [{"field": "source_trust", "operator": "ne", "value": "trusted_system"}],
            "order_by": ["created_at"],
        }
    )
    assert program.predicates[0].operator == "ne"
    assert program.order_by == ("created_at",)
    assert (
        SearchProgram.parse(
            {
                "channels": [{"channel": "exact", "value": "x"}],
                "predicates": [{"field": "agent_thread_id", "operator": "exists", "value": True}],
            }
        )
        .predicates[0]
        .operator
        == "exists"
    )


def test_the_module_cannot_evaluate_anything() -> None:
    """Structural proof: the module cannot turn a value into code.

    A text search would be wrong here: the module deliberately *names* `eval` and `exec(` in the
    marker list of inputs it refuses. Parsing the module proves there is no evaluator call and no
    evaluator import.
    """
    import ast

    tree = ast.parse(pathlib.Path("intelligence/context/search.py").read_text())
    forbidden_calls = {"eval", "exec", "compile", "__import__", "system", "popen"}
    forbidden_modules = {"subprocess", "pickle", "os", "marshal", "sqlite3"}
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            name = getattr(node.func, "id", None) or getattr(node.func, "attr", None)
            assert name not in forbidden_calls, f"the module must not call {name}"
        if isinstance(node, ast.Import):
            for alias in node.names:
                assert alias.name.split(".")[0] not in forbidden_modules, f"no {alias.name} import"
        if isinstance(node, ast.ImportFrom):
            root = (node.module or "").split(".")[0]
            assert root not in forbidden_modules, f"no {root} import"
