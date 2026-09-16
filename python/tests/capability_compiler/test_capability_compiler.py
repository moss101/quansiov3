"""CAP-004: compiler ingestion — provenance, credential exclusion, tenant isolation.

The suite drives the shipped `compile_pack` with authoritative-looking sources
and the adversarial ones: a credential-laden SOP and a cross-tenant document
must be refused, and every surviving requirement must carry provenance back to
its source span. Output is a candidate draft — never executable.
"""

from __future__ import annotations

import pytest

from intelligence.capability_compiler import (
    CompilerError,
    IngestionSource,
    compile_pack,
    scan_for_credentials,
)

TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0CAP004"
OTHER = "tn_01J8Z3K6F1N8VQ2X5W9Y0CAP999"

SOP = (
    "Deploying the service. Step 1: run the migration tool. "
    "Then verify that the health endpoint responds. "
    "Data retention must not exceed ninety days."
)
POLICY = (
    "Access policy. Production access is prohibited without an approval. "
    "Every request must reference a ticket."
)
API_DOC = "The public API is an endpoint at /v1/events. Check that the response is paginated."


def sources() -> list[IngestionSource]:
    return [
        IngestionSource("src_sop", TENANT, "sop", SOP),
        IngestionSource("src_policy", TENANT, "policy", POLICY),
        IngestionSource("src_api", TENANT, "api", API_DOC),
    ]


def test_ingestion_provenance_traces_output_to_source_spans() -> None:
    """Named test: ingestion provenance. Every requirement names source and span."""
    draft = compile_pack("onboarding", TENANT, sources())
    assert draft.status == "candidate", "compiler output is a candidate, never executable"
    assert set(draft.sources) == {"src_sop", "src_policy", "src_api"}
    assert draft.requirements, "decomposition produced requirements"
    for requirement in draft.requirements:
        source_id, start, end = (
            requirement.source_id,
            requirement.span[0],
            requirement.span[1],
        )
        original = next(item.content for item in sources() if item.source_id == source_id)
        assert original[start:end] == requirement.text, "span must quote the source verbatim"
        assert len(requirement.provenance_digest) == 64
    kinds = {item.kind for item in draft.requirements}
    assert {"policy", "process", "evaluation"} <= kinds
    # The content digest pins what qualification would review.
    assert len(draft.content_digest()) == 64
    mapping = draft.as_mapping()
    assert mapping["status"] == "candidate"


def test_credential_leak_is_refused_at_ingestion() -> None:
    """Named test: credential leak. Credential-shaped content never becomes a pack."""
    for payload in (
        "Use this key: sk-abcdefghijklmnopqrstuvwx to call the endpoint.",
        "Upload with ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234.",
        "Sign with -----BEGIN RSA PRIVATE KEY----- ...",
        "The password: hunter2 is required for the dashboard.",
    ):
        assert scan_for_credentials(payload), f"scanner missed {payload!r}"
        with pytest.raises(CompilerError) as raised:
            compile_pack(
                "leaky",
                TENANT,
                [IngestionSource("src_leak", TENANT, "sop", payload)],
            )
        assert raised.value.code == "compiler.credential"
    # A clean source passes the same scanner.
    assert scan_for_credentials(SOP) == []


def test_tenant_isolation_excludes_cross_tenant_sources() -> None:
    """Named test: tenant isolation. A cross-tenant source is refused, never merged."""
    mixed = [
        IngestionSource("src_mine", TENANT, "sop", SOP),
        IngestionSource("src_theirs", OTHER, "policy", POLICY),
    ]
    with pytest.raises(CompilerError) as raised:
        compile_pack("mixed", TENANT, mixed)
    assert raised.value.code == "compiler.tenant"
    assert "src_theirs" in raised.value.detail

    # An empty compilation is refused too.
    with pytest.raises(CompilerError) as raised:
        compile_pack("empty", TENANT, [])
    assert raised.value.code == "compiler.empty"


def test_output_is_not_executable() -> None:
    """The draft is inert data: no callables, status candidate, digest-pinned."""
    draft = compile_pack("onboarding", TENANT, sources())
    mapping = draft.as_mapping()
    for requirement in mapping["requirements"]:  # type: ignore[union-attr]
        for value in requirement.values():  # type: ignore[union-attr]
            assert not callable(value)
    before = draft.content_digest()
    draft.requirements.pop()
    assert draft.content_digest() != before, "the digest must move with the content"
