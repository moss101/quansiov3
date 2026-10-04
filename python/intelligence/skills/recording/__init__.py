"""Skill recording (ADP-003, DOMAIN.md §11.7): compile a demonstration into a skill proposal.

The plane's half of the recording flow. It reads the text-only projection of one finished, redacted
recording (untrusted data), and proposes a skill: applications by bundle id, controls by accessibility
attributes, input slots for the person to classify, conditions and recovery, tools before pixels. It
returns a proposal. It has no write path: the Rust control plane validates it, keeps it and, if the person
submits it, registers it as ``DRAFT``.
"""

from __future__ import annotations

from intelligence.skills.recording.compiler import (
    CompileSettings,
    RecordingCompileError,
    assert_redacted,
    compile_recording,
)
from intelligence.skills.recording.proposal import (
    FORBIDDEN_KEYS,
    Limits,
    ProposalError,
    ToolInfo,
    find_coordinates,
    validate_proposal,
)
from intelligence.skills.recording.redact import (
    CATEGORIES,
    RULE_SET,
    redact_field_text,
    redact_text,
    scan_text,
)

__all__ = [
    "CATEGORIES",
    "FORBIDDEN_KEYS",
    "RULE_SET",
    "CompileSettings",
    "Limits",
    "ProposalError",
    "RecordingCompileError",
    "ToolInfo",
    "assert_redacted",
    "compile_recording",
    "find_coordinates",
    "redact_field_text",
    "redact_text",
    "scan_text",
    "validate_proposal",
]
