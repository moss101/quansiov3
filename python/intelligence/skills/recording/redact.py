"""The recording redaction rule set ``v1`` (DOMAIN.md §11.7 C), as the plane applies it.

The third place the rule set runs: the person's recorder redacts before it builds an archive, the server
re-checks before it stores one, and the compiler re-checks before it reads one. All three are pinned to one
hand-written corpus (``tests/contract/fixtures/recording/redaction_corpus.json``); the patterns below use
only the regular-expression vocabulary the three engines read the same way (no lookaround, case folding
only), and the order, markers and field rules mirror ``crates/server/.../recording/redact.rs`` line for
line. A compiler that reads a recording therefore reads text with nothing left in it for redaction to
remove, and it fails closed (``RECORDING_UNREDACTED``) on text that has.
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from dataclasses import dataclass
from itertools import pairwise

RULE_SET = "v1"

API_KEY = "api_key"
BEARER_TOKEN = "bearer_token"
CARD_NUMBER = "card_number"
VERIFICATION_CODE = "verification_code"
PASSWORD_FIELD = "password_field"

#: Every category, in rule order.
CATEGORIES: tuple[str, ...] = (API_KEY, BEARER_TOKEN, CARD_NUMBER, VERIFICATION_CODE, PASSWORD_FIELD)

_TRIGGERS = r"(?:otp|passcode|2fa|mfa|pin|code|verification|one[- ]?time|confirmation)"

_PEM_BLOCK = re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----")
_PEM_HEADER = re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")
_API_KEY_SHAPES = tuple(
    re.compile(pattern)
    for pattern in (
        r"\b(?:sk|pk|rk|whsec)[_-][A-Za-z0-9_-]{16,}",
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        r"\bgh[pousr]_[A-Za-z0-9]{30,}",
        r"\bgithub_pat_[A-Za-z0-9_]{40,}",
        r"\bxox[abprs]-[A-Za-z0-9-]{10,}",
        r"\bAIza[0-9A-Za-z_-]{35,}",
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    )
)
_LONG_TOKEN = re.compile(r"\b[A-Za-z0-9_-]{40,}\b")
_AUTHORIZATION = re.compile(
    r"\bauthorization\s*[:=]\s*(?:(?:bearer|basic|token)\s+)?[A-Za-z0-9._~+/=-]{8,}", re.IGNORECASE
)
_BEARER = re.compile(r"\bbearer\s+[A-Za-z0-9._~+/=-]{8,}", re.IGNORECASE)
_CARD = re.compile(r"\d(?:[ -]?\d){12,}")
_CODE_AFTER_WORD = re.compile(rf"\b{_TRIGGERS}\b[^0-9\n]{{0,40}}\b\d{{4,8}}\b", re.IGNORECASE)
_CODE_BEFORE_WORD = re.compile(rf"\b\d{{4,8}}\b[^0-9\n]{{0,40}}\b{_TRIGGERS}\b", re.IGNORECASE)

_SECURE_ROLE = "AXSecureTextField"


def marker(category: str) -> str:
    """What replaces a removed value."""
    return f"[REDACTED:{category}]"


def _replace(text: str, pattern: re.Pattern[str], category: str, counts: dict[str, int]) -> str:
    out, found = pattern.subn(marker(category), text)
    if found:
        counts[category] = counts.get(category, 0) + found
    return out


def _replace_long_tokens(text: str, counts: dict[str, int]) -> str:
    found = 0

    def swap(match: re.Match[str]) -> str:
        nonlocal found
        token = match.group(0)
        has_letter = any(c.isascii() and c.isalpha() for c in token)
        has_digit = any(c.isascii() and c.isdigit() for c in token)
        if has_letter and has_digit:
            found += 1
            return marker(API_KEY)
        return token

    out = _LONG_TOKEN.sub(swap, text)
    if found:
        counts[API_KEY] = counts.get(API_KEY, 0) + found
    return out


def redact_text(text: str) -> tuple[str, dict[str, int]]:
    """Redact free text: the text with every match replaced, and the count per category found."""
    counts: dict[str, int] = {}
    out = _replace(text, _PEM_BLOCK, API_KEY, counts)
    out = _replace(out, _PEM_HEADER, API_KEY, counts)
    for shape in _API_KEY_SHAPES:
        out = _replace(out, shape, API_KEY, counts)
    out = _replace_long_tokens(out, counts)
    out = _replace(out, _AUTHORIZATION, BEARER_TOKEN, counts)
    out = _replace(out, _BEARER, BEARER_TOKEN, counts)
    out = _replace(out, _CARD, CARD_NUMBER, counts)
    out = _replace(out, _CODE_AFTER_WORD, VERIFICATION_CODE, counts)
    out = _replace(out, _CODE_BEFORE_WORD, VERIFICATION_CODE, counts)
    return out, counts


def scan_text(text: str) -> dict[str, int]:
    """What a text still holds that redaction would remove. Empty for any redacted text."""
    return redact_text(text)[1]


def _label_tokens(label: str) -> list[str]:
    spaced: list[str] = []
    previous = ""
    for char in label:
        if previous and ("a" <= previous <= "z" or "0" <= previous <= "9") and "A" <= char <= "Z":
            spaced.append(" ")
        spaced.append(char)
        previous = char
    return [token.lower() for token in re.split(r"[^A-Za-z0-9]+", "".join(spaced)) if token]


def _has_pair(tokens: list[str], pairs: tuple[tuple[str, str], ...]) -> bool:
    return any(pair in pairs for pair in pairwise(tokens))


def _label_category(tokens: list[str]) -> str | None:
    if any(
        t in {"otp", "2fa", "mfa", "verification", "authenticator", "onetime"} for t in tokens
    ) or _has_pair(
        tokens,
        (
            ("one", "time"),
            ("security", "code"),
            ("auth", "code"),
            ("authentication", "code"),
            ("confirmation", "code"),
            ("sms", "code"),
            ("login", "code"),
        ),
    ):
        return VERIFICATION_CODE
    if any(t in {"cvv", "cvc", "ccnum", "cardnumber"} for t in tokens) or _has_pair(
        tokens,
        (("card", "number"), ("card", "num"), ("credit", "card"), ("debit", "card"), ("cc", "number")),
    ):
        return CARD_NUMBER
    if "apikey" in tokens or _has_pair(
        tokens,
        (("api", "key"), ("api", "token"), ("access", "token"), ("auth", "token"), ("private", "key")),
    ):
        return API_KEY
    if any(t in {"password", "passwd", "pwd", "passphrase", "passcode", "secret"} for t in tokens):
        return PASSWORD_FIELD
    return None


@dataclass(frozen=True, slots=True)
class FieldRule:
    """What entering text into a control means for that text."""

    secure: bool = False
    category: str | None = None


def field_rule(attrs: Mapping[str, object]) -> FieldRule | None:
    """What a control's own attributes say about text entered into it, if anything."""
    if attrs.get("ax_role") == _SECURE_ROLE or attrs.get("ax_subrole") == _SECURE_ROLE:
        return FieldRule(secure=True)
    found: set[str] = set()
    for key in ("ax_title", "ax_identifier", "ax_description"):
        label = attrs.get(key)
        if isinstance(label, str):
            category = _label_category(_label_tokens(label))
            if category is not None:
                found.add(category)
    for category in CATEGORIES:
        if category != BEARER_TOKEN and category in found:
            return FieldRule(category=category)
    return None


def redact_field_text(attrs: Mapping[str, object], text: str) -> tuple[str | None, dict[str, int]]:
    """Redact text entered into a control: dropped (``None``) in a secure field, the category marker in a
    labelled one, otherwise the ordinary text rules. Empty text is nothing and counts nothing."""
    if not text:
        return "", {}
    rule = field_rule(attrs)
    if rule is not None and rule.secure:
        return None, {PASSWORD_FIELD: 1}
    if rule is not None and rule.category is not None:
        return marker(rule.category), {rule.category: 1}
    return redact_text(text)
