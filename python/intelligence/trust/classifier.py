"""Deterministic, non-LLM content trust labelling and injection heuristics (DOMAIN.md §12).

`classify` maps a source kind to the canonical trust level and flags content that carries
instructions aimed at the agent. It performs no model call, no I/O and no state change: the
result is a pure function of its inputs, so it is reproducible and cheap to call per segment.

Trust levels mirror `quansio.v1.trust.TrustLevel` (DOMAIN.md §12). The intelligence boundary
may only import its own generated service package (`scripts/ci/arch_check.py`,
`python-authority-write`), so the values are mirrored here and pinned to the generated
contract by `python/tests/intelligence/test_trust_classifier.py`.

Instructions inside content are data, never intent (AGENTS.md invariant 17): this module
labels and flags, it never authorizes. Escalation, exfiltration guards and truncated
rendering are Rust policy / INT-012 work.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import IntEnum


# DOMAIN.md §12 trust levels, in the numeric order of quansio.v1.trust.TrustLevel.
class TrustLevel(IntEnum):
    """Mirror of `quansio.v1.trust.TrustLevel`; values are part of the wire contract."""

    TRUSTED_SYSTEM = 1
    TRUSTED_USER = 2
    VERIFIED_KNOWLEDGE = 3
    AGENT_GENERATED = 4
    UNTRUSTED_EXTERNAL = 5


# Source kinds from DOMAIN.md §11.2 (ContextProjection segments) and §12 (untrusted sources).
# An unknown or absent kind fails closed to UNTRUSTED_EXTERNAL.
SOURCE_KIND_TRUST: dict[str, TrustLevel] = {
    "system": TrustLevel.TRUSTED_SYSTEM,
    "policy": TrustLevel.TRUSTED_SYSTEM,
    "tool_definition": TrustLevel.TRUSTED_SYSTEM,
    "thread": TrustLevel.TRUSTED_USER,
    "user": TrustLevel.TRUSTED_USER,
    "work": TrustLevel.TRUSTED_USER,
    "knowledge": TrustLevel.VERIFIED_KNOWLEDGE,
    "skill": TrustLevel.VERIFIED_KNOWLEDGE,
    "memory": TrustLevel.AGENT_GENERATED,
    "assistant": TrustLevel.AGENT_GENERATED,
    "agent": TrustLevel.AGENT_GENERATED,
    "artifact": TrustLevel.UNTRUSTED_EXTERNAL,
    "attachment": TrustLevel.UNTRUSTED_EXTERNAL,
    "connector": TrustLevel.UNTRUSTED_EXTERNAL,
    "document": TrustLevel.UNTRUSTED_EXTERNAL,
    "email": TrustLevel.UNTRUSTED_EXTERNAL,
    "external": TrustLevel.UNTRUSTED_EXTERNAL,
    "file": TrustLevel.UNTRUSTED_EXTERNAL,
    "search": TrustLevel.UNTRUSTED_EXTERNAL,
    "tool_result": TrustLevel.UNTRUSTED_EXTERNAL,
    "upload": TrustLevel.UNTRUSTED_EXTERNAL,
    "web": TrustLevel.UNTRUSTED_EXTERNAL,
}

# Sources whose instructions are intent, not injection (DOMAIN.md §12: instructions honoured).
# The runtime renders system/policy text; in-thread user messages are authored by workspace
# members. Heuristics are not applied to them, so a legitimate "ignore the previous plan"
# from a user is never reported as an attack.
_INSTRUCTION_SOURCES = frozenset({TrustLevel.TRUSTED_SYSTEM, TrustLevel.TRUSTED_USER})

# Untrusted content is scanned only up to this many characters; a hostile segment cannot
# make classification unbounded work.
_MAX_SCAN_CHARS = 65_536

# Zero-width and BOM characters are dropped before matching so trivial obfuscation
# ("ig\u200bnore previous instructions") does not evade the heuristics.
_INVISIBLE_RE = re.compile("[\u200b\u200c\u200d\u2060\ufeff]")
_WHITESPACE_RE = re.compile(r"\s+")

# Deterministic pattern set. Names are stable identifiers surfaced to callers.
_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    (
        "instruction_override",
        re.compile(
            r"\b(?:ignore|disregard|forget|override|bypass)\b[^\n]{0,40}?"
            r"\b(?:previous|prior|earlier|preceding|above|system|safety|all)\b[^\n]{0,20}?"
            r"\b(?:instructions?|prompts?|rules?|guidelines?)\b"
        ),
    ),
    (
        "system_prompt_disclosure",
        re.compile(
            r"\b(?:reveal|print|show|repeat|output|disclose|expose|leak)\b[^\n]{0,30}?"
            r"\b(?:your|the)\b[^\n]{0,10}?\b(?:system|developer|hidden|initial|original)\b"
            r"[^\n]{0,10}?\b(?:prompt|instructions?|message|rules?)\b"
        ),
    ),
    (
        "role_reassignment",
        re.compile(
            r"\byou\s+are\s+now\b|"
            r"\bact\s+as\s+(?:an?\s+)?(?:admin|administrator|root|developer|unrestricted|jailbroken)\b"
        ),
    ),
    (
        "new_instructions",
        re.compile(r"\bnew\s+(?:instructions?|directives?|orders?|rules?)\s*[:.]"),
    ),
    (
        "command_invocation",
        re.compile(
            r"\b(?:run|execute|invoke|call)\b[^\n]{0,30}?"
            r"\b(?:command|shell|script|powershell|bash|terminal|tool)\b"
        ),
    ),
    (
        "credential_exfiltration",
        re.compile(
            r"\b(?:send|post|upload|exfiltrate|forward|leak|transmit|share|reveal)\b[^\n]{0,60}?"
            r"\b(?:api[\s_-]?keys?|access[\s_-]?tokens?|tokens?|passwords?|passphrases?|secrets?"
            r"|credentials?|private[\s_-]?keys?|\.env)\b"
        ),
    ),
    (
        "approval_bypass",
        re.compile(
            r"\b(?:skip|bypass|ignore|without)\b[^\n]{0,30}?"
            r"\b(?:approval|authorisation|authorization|permission|policy|confirmation)\b"
        ),
    ),
    (
        "concealment",
        re.compile(
            r"\b(?:do\s+not|don't|never)\b[^\n]{0,30}?\b(?:tell|inform|notify|mention|warn)\b"
            r"[^\n]{0,20}?\b(?:the\s+)?(?:user|owner|operator|human)\b"
        ),
    ),
    (
        "encoded_execution",
        re.compile(r"\b(?:eval|exec)\s*\(|\bbase64\s+(?:decode|-d)\b"),
    ),
)

PATTERN_NAMES: tuple[str, ...] = tuple(name for name, _ in _PATTERNS)


@dataclass(frozen=True, slots=True)
class TrustClassification:
    """Trust label and injection verdict for one piece of content."""

    trust_level: TrustLevel
    injection_suspected: bool
    matched_patterns: tuple[str, ...]


def normalize(content: str) -> str:
    """Drop invisible characters, collapse whitespace and lower-case for matching."""
    trimmed = content[:_MAX_SCAN_CHARS]
    return _WHITESPACE_RE.sub(" ", _INVISIBLE_RE.sub("", trimmed)).strip().lower()


def detect_injection(content: str) -> tuple[str, ...]:
    """Names of the heuristic patterns that matched, sorted and de-duplicated."""
    normalized = normalize(content)
    if not normalized:
        return ()
    return tuple(sorted({name for name, pattern in _PATTERNS if pattern.search(normalized)}))


def classify(source_kind: str, content: str) -> TrustClassification:
    """Label `content` from `source_kind` and flag suspected injection.

    Unknown source kinds fail closed to `UNTRUSTED_EXTERNAL`. Sources whose instructions are
    intent (`TRUSTED_SYSTEM`, `TRUSTED_USER`) are never flagged as injection.
    """
    trust_level = SOURCE_KIND_TRUST.get(source_kind.strip().lower(), TrustLevel.UNTRUSTED_EXTERNAL)
    if trust_level in _INSTRUCTION_SOURCES:
        return TrustClassification(trust_level, False, ())
    matched = detect_injection(content)
    return TrustClassification(trust_level, bool(matched), matched)
