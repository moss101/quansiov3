"""The typed SearchProgram (DOMAIN.md §11.3, INT-005).

A search program is **data**, never a string the runtime evaluates. Every term, predicate and
order key is a declared value with a closed operator vocabulary, so there is no place for an
executable or arbitrary predicate to hide — the acceptance rule for this task. Validation is
fail-closed: an unknown channel, field, operator, key or value shape is refused with
`VALIDATION_SCHEMA`, never ignored.

The canonical form is deterministic: programs that describe the same search serialize to the same
bytes whatever order their predicates were written in, which is what lets a projection record the
program it was built from and a cache key it.
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from enum import StrEnum
from typing import Any

#: The channels DOMAIN.md §11.3 names.
CHANNELS: tuple[str, ...] = ("exact", "lexical", "semantic", "graph", "history", "memory")

#: The closed operator vocabulary. There is deliberately no string-expression operator.
OPERATORS: tuple[str, ...] = (
    "eq",
    "ne",
    "in",
    "gt",
    "gte",
    "lt",
    "lte",
    "prefix",
    "contains",
    "exists",
)

#: Fields a predicate may address, with the operators and value shapes each accepts.
FIELD_OPERATORS: Mapping[str, tuple[str, ...]] = {
    "workspace_id": ("eq", "ne"),
    "work_node_id": ("eq", "ne", "in"),
    "artifact_role": ("eq", "ne", "in", "prefix"),
    "run_id": ("eq", "ne", "in"),
    "agent_thread_id": ("eq", "ne", "exists"),
    "source_trust": ("eq", "ne", "in"),
    "kind": ("eq", "ne", "in"),
    "created_at": ("gt", "gte", "lt", "lte"),
    "title": ("contains", "prefix", "eq"),
    "status": ("eq", "ne", "in"),
}

#: Value shapes per operator, used to refuse a predicate whose value cannot mean what it says.
OPERATOR_VALUE_KINDS: Mapping[str, str] = {
    "eq": "any",
    "ne": "any",
    "in": "list",
    "gt": "number-or-string",
    "gte": "number-or-string",
    "lt": "number-or-string",
    "lte": "number-or-string",
    "prefix": "string",
    "contains": "string",
    "exists": "boolean",
}

#: Fragments that mark a value as an attempted expression rather than a literal.
EXPRESSION_MARKERS: tuple[str, ...] = (
    "(",
    ")",
    ";",
    "--",
    "/*",
    "$(",
    "${",
    "&&",
    "||",
    "==",
    "!=",
    "select ",
    "drop ",
    "union ",
    "eval",
    "exec(",
    "`",
)

TERM_KEYS: Mapping[str, tuple[str, ...]] = {
    "exact": ("value",),
    "lexical": ("text",),
    "semantic": ("text",),
    "graph": ("node_id",),
    "history": ("run_id",),
    "memory": ("scope",),
}


class Channel(StrEnum):
    """One retrieval channel."""

    EXACT = "exact"
    LEXICAL = "lexical"
    SEMANTIC = "semantic"
    GRAPH = "graph"
    HISTORY = "history"
    MEMORY = "memory"


class SearchProgramError(ValueError):
    """A refused search program; carries the rule that refused it."""

    def __init__(self, rule_id: str, detail: str) -> None:
        super().__init__(f"VALIDATION_SCHEMA: {detail} (rule {rule_id})")
        self.rule_id = rule_id
        self.detail = detail
        self.code = "VALIDATION_SCHEMA"


RULE_CHANNEL = "program.channel"
RULE_KNOWN_KEYS = "program.known_keys"
RULE_TERM_SHAPE = "program.term_shape"
RULE_FIELD = "program.field"
RULE_OPERATOR = "program.operator"
RULE_VALUE = "program.value"
RULE_LITERAL = "program.literal_only"
RULE_LIMIT = "program.limit"


@dataclass(frozen=True, slots=True)
class ChannelTerm:
    """One channel's query term: a literal value for that channel, nothing more."""

    channel: Channel
    value: str
    limit: int = 10


@dataclass(frozen=True, slots=True)
class Predicate:
    """A typed filter: a declared field, a declared operator and a literal value."""

    field: str
    operator: str
    value: Any


@dataclass(frozen=True, slots=True)
class SearchProgram:
    """A validated search over the canonical corpus, in one canonical form."""

    channels: tuple[ChannelTerm, ...]
    predicates: tuple[Predicate, ...] = ()
    order_by: tuple[str, ...] = ()
    limit: int = 20

    def canonical(self) -> dict[str, Any]:
        """The canonical mapping: keys sorted, predicates ordered deterministically.

        Two programs that describe the same search produce the same mapping, whatever order their
        parts were written in, so a projection can record the program it was built from and a cache
        key it.
        """
        channels = sorted(
            (
                {"channel": term.channel.value, "value": term.value, "limit": term.limit}
                for term in self.channels
            ),
            key=lambda item: (item["channel"], item["value"], item["limit"]),
        )
        predicates = sorted(
            (
                {"field": predicate.field, "operator": predicate.operator, "value": predicate.value}
                for predicate in self.predicates
            ),
            key=lambda item: (item["field"], item["operator"], json.dumps(item["value"], sort_keys=True)),
        )
        return {
            "channels": channels,
            "order_by": list(self.order_by),
            "predicates": predicates,
            "limit": self.limit,
        }

    def canonical_json(self) -> str:
        """The canonical form as stable JSON (sorted keys, no whitespace)."""
        return json.dumps(self.canonical(), sort_keys=True, separators=(",", ":"))

    def to_json(self) -> str:
        """The bytes a caller stores, so the program is reproducible."""
        return self.canonical_json()

    @classmethod
    def parse(cls, payload: Mapping[str, Any] | str) -> SearchProgram:
        """Parse and validate a program, refusing anything outside the vocabulary.

        Raises [`SearchProgramError`] (`VALIDATION_SCHEMA`) for an unknown channel, key, field,
        operator or value shape, and for any value that looks like an attempted expression.
        """
        raw = json.loads(payload) if isinstance(payload, str) else dict(payload)
        unknown = sorted(set(raw) - {"channels", "predicates", "order_by", "limit"})
        if unknown:
            raise SearchProgramError(RULE_KNOWN_KEYS, f"unknown program keys: {', '.join(unknown)}")

        channels_raw = raw.get("channels", [])
        if not isinstance(channels_raw, Sequence) or isinstance(channels_raw, (str, bytes)):
            raise SearchProgramError(RULE_CHANNEL, "channels must be a list")
        channels: list[ChannelTerm] = []
        for entry in channels_raw:
            if not isinstance(entry, Mapping):
                raise SearchProgramError(RULE_TERM_SHAPE, "each channel term must be an object")
            channel_name = entry.get("channel")
            try:
                channel = Channel(str(channel_name))
            except ValueError as error:
                raise SearchProgramError(
                    RULE_CHANNEL, f"{channel_name!r} is not one of {list(CHANNELS)}"
                ) from error
            allowed = {"channel", "limit", *TERM_KEYS[channel.value]}
            extra = sorted(set(entry) - allowed)
            if extra:
                raise SearchProgramError(RULE_TERM_SHAPE, f"unknown term keys: {', '.join(extra)}")
            value_key = TERM_KEYS[channel.value][0]
            value = entry.get(value_key)
            if not isinstance(value, str) or not value.strip():
                raise SearchProgramError(
                    RULE_TERM_SHAPE, f"a {channel.value} term needs a non-empty {value_key}"
                )
            _refuse_expression(value, RULE_LITERAL)
            limit = entry.get("limit", 10)
            if not isinstance(limit, int) or isinstance(limit, bool) or limit < 1:
                raise SearchProgramError(RULE_LIMIT, "a term limit must be a positive integer")
            channels.append(ChannelTerm(channel=channel, value=value, limit=limit))

        predicates_raw = raw.get("predicates", [])
        if not isinstance(predicates_raw, Sequence) or isinstance(predicates_raw, (str, bytes)):
            raise SearchProgramError(RULE_KNOWN_KEYS, "predicates must be a list")
        predicates: list[Predicate] = []
        for entry in predicates_raw:
            if not isinstance(entry, Mapping):
                raise SearchProgramError(RULE_TERM_SHAPE, "each predicate must be an object")
            extra = sorted(set(entry) - {"field", "operator", "value"})
            if extra:
                raise SearchProgramError(RULE_KNOWN_KEYS, f"unknown predicate keys: {', '.join(extra)}")
            field = entry.get("field")
            operator = entry.get("operator")
            if not isinstance(field, str) or field not in FIELD_OPERATORS:
                raise SearchProgramError(
                    RULE_FIELD, f"{field!r} is not a declared field; the vocabulary is closed"
                )
            if not isinstance(operator, str) or operator not in FIELD_OPERATORS[field]:
                raise SearchProgramError(RULE_OPERATOR, f"operator {operator!r} is not allowed on {field!r}")
            value = entry.get("value")
            _check_value(operator, value)
            predicates.append(Predicate(field=field, operator=operator, value=value))

        order_by = raw.get("order_by", [])
        if not isinstance(order_by, Sequence) or isinstance(order_by, (str, bytes)):
            raise SearchProgramError(RULE_KNOWN_KEYS, "order_by must be a list")
        for key in order_by:
            if key not in FIELD_OPERATORS:
                raise SearchProgramError(RULE_FIELD, f"cannot order by {key!r}: it is not a declared field")
        limit = raw.get("limit", 20)
        if not isinstance(limit, int) or isinstance(limit, bool) or limit < 1:
            raise SearchProgramError(RULE_LIMIT, "the program limit must be a positive integer")
        if not channels:
            raise SearchProgramError(RULE_CHANNEL, "a program needs at least one channel")
        return cls(
            channels=tuple(channels),
            predicates=tuple(predicates),
            order_by=tuple(str(key) for key in order_by),
            limit=limit,
        )


def _check_value(operator: str, value: Any) -> None:
    """Refuse a value whose shape cannot mean what the operator says."""
    kind = OPERATOR_VALUE_KINDS[operator]
    if kind == "list":
        if not isinstance(value, list) or not value:
            raise SearchProgramError(RULE_VALUE, f"{operator} needs a non-empty list")
        for item in value:
            _refuse_expression(item, RULE_LITERAL)
        return
    if kind == "string":
        if not isinstance(value, str) or not value.strip():
            raise SearchProgramError(RULE_VALUE, f"{operator} needs a non-empty string")
        _refuse_expression(value, RULE_LITERAL)
        return
    if kind == "boolean":
        if not isinstance(value, bool):
            raise SearchProgramError(RULE_VALUE, f"{operator} needs a boolean")
        return
    if kind == "number-or-string":
        if isinstance(value, bool) or not isinstance(value, (int, float, str)):
            raise SearchProgramError(RULE_VALUE, f"{operator} needs a number or a timestamp string")
        _refuse_expression(value, RULE_LITERAL)
        return
    _refuse_expression(value, RULE_LITERAL)


def _refuse_expression(value: Any, rule_id: str) -> None:
    """Refuse a value that reads like an expression rather than a literal.

    This is the acceptance rule: a search program is data, so a value carrying expression syntax —
    parentheses, statement separators, comment markers, shell substitution, boolean operators — is
    refused instead of being handed to anything that might interpret it.
    """
    if not isinstance(value, str):
        return
    lowered = value.lower()
    for marker in EXPRESSION_MARKERS:
        if marker in lowered:
            raise SearchProgramError(
                rule_id,
                f"value {value!r} contains {marker!r}; search values are literals, not expressions",
            )
