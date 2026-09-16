"""Capability Compiler (CAP-004): ingestion and decomposition into a candidate pack.

The draft is non-executable data with per-element provenance; promotion is
CAP-005's. Credentials and private cross-tenant material are refused at
ingestion.
"""

from __future__ import annotations

from .compiler import (
    CREDENTIAL_PATTERNS,
    CompilerError,
    IngestionSource,
    PackDraft,
    Requirement,
    compile_pack,
    scan_for_credentials,
)

__all__ = [
    "CREDENTIAL_PATTERNS",
    "CompilerError",
    "IngestionSource",
    "PackDraft",
    "Requirement",
    "compile_pack",
    "scan_for_credentials",
]
