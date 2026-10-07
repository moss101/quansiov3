"""Capability Compiler (CAP-004): ingestion and decomposition into a candidate pack.

The draft is non-executable data with per-element provenance; promotion is
CAP-005's. Credentials and private cross-tenant material are refused at
ingestion. `install` (ADP-008) is the plane's half of installing a pack by
content-hash revision: it verifies and compiles, and writes nothing.
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
from .install import (
    InstallSettings,
    PackCompilation,
    ToolVisibility,
    compile_install,
    manifest_digest,
    revision_of,
)

__all__ = [
    "CREDENTIAL_PATTERNS",
    "CompilerError",
    "IngestionSource",
    "InstallSettings",
    "PackCompilation",
    "PackDraft",
    "Requirement",
    "ToolVisibility",
    "compile_install",
    "compile_pack",
    "manifest_digest",
    "revision_of",
    "scan_for_credentials",
]
