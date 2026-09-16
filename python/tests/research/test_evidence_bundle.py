"""CAP-002: EvidenceBundle — broken citations, claim coverage, provider survival.

The suite drives the shipped `EvidenceBundle.build` and `verify`: a broken
citation, an uncited claim, and a digest drift each fail; the same bundle
re-verifies after a reload regardless of the model route/provider that made it.
"""

from __future__ import annotations

import hashlib

import pytest

from intelligence.research import (
    CitedClaim,
    EvidenceBundle,
    EvidenceDescriptor,
    EvidenceError,
    verify,
)

DESC_POLICY = EvidenceDescriptor(
    source_id="src_policy",
    uri="https://corp.test/policy",
    content_digest=hashlib.sha256(b"Logs are retained for ninety days.").hexdigest(),
    text="Logs are retained for ninety days.",
    retrieved_at="2026-09-16T11:15:00Z",
)
DESC_RUNBOOK = EvidenceDescriptor(
    source_id="src_runbook",
    uri="https://corp.test/runbook",
    content_digest=hashlib.sha256(b"Deletion happens after the window.").hexdigest(),
    text="Deletion happens after the window.",
    retrieved_at="2026-09-16T11:15:00Z",
)


def bundle() -> EvidenceBundle:
    return EvidenceBundle.build(
        "evb_01J8Z3K6F1N8VQ2X5W9Y0BUNDLE1",
        claims=(
            CitedClaim("claim_retention", "Logs are retained for ninety days.", ("src_policy",)),
            CitedClaim(
                "claim_deletion",
                "Deletion happens after the window.",
                ("src_policy", "src_runbook"),
            ),
        ),
        descriptors=(DESC_POLICY, DESC_RUNBOOK),
        model_route="anthropic-sonnet",
        provider="anthropic",
    )


def test_broken_citation_is_refused_at_build_and_at_verify() -> None:
    with pytest.raises(EvidenceError) as raised:
        EvidenceBundle.build(
            "evb_01J8Z3K6F1N8VQ2X5W9Y0BUNDLE1",
            claims=(CitedClaim("claim_x", "text", ("src_missing",)),),
            descriptors=(DESC_POLICY,),
        )
    assert "not in the bundle" in raised.value.detail

    ok, failures = verify(
        bundle(),
        claims=(CitedClaim("claim_retention", "Logs are retained for ninety days.", ("src_broken",)),),
    )
    assert not ok
    assert any("broken citation src_broken" in item for item in failures)


def test_claim_coverage_requires_citations() -> None:
    with pytest.raises(EvidenceError) as raised:
        EvidenceBundle.build(
            "evb_01J8Z3K6F1N8VQ2X5W9Y0BUNDLE1",
            claims=(CitedClaim("claim_none", "text", ()),),
            descriptors=(DESC_POLICY,),
        )
    assert "cites nothing" in raised.value.detail
    ok, failures = verify(
        bundle(),
        claims=(CitedClaim("claim_retention", "text", ()),),
    )
    assert not ok
    assert any("no citations" in item for item in failures)


def test_bundle_reloads_and_survives_a_provider_change() -> None:
    """Named test: bundle reload. Evidence, not the provider, is the authority."""
    original = bundle()
    mapping = original.as_mapping()
    # Rebuild from the serialized shape with a different model route/provider.
    reloaded = EvidenceBundle.build(
        str(mapping["bundle_id"]),
        claims=[
            CitedClaim(item["claim_id"], item["text"], tuple(item["citations"]))
            for item in mapping["claims"]  # type: ignore[union-attr]
        ],
        descriptors=[
            EvidenceDescriptor(
                source_id=item["source_id"],
                uri=item["uri"],
                content_digest=item["content_digest"],
                text=item["text"],
                retrieved_at=item["retrieved_at"],
            )
            for item in (d.as_mapping() | {"text": d.text} for d in original.descriptors)
        ],
        model_route="openai-gpt",
        provider="openai",
    )
    assert reloaded.bundle_digest == original.bundle_digest, "digest must be provider-independent"
    assert reloaded.provider != original.provider

    # The same claims verify identically under the new provider.
    ok, failures = verify(reloaded, original.claims)
    assert ok, failures

    # Digest drift (evidence text changed underneath) fails verification.
    drifted = EvidenceDescriptor(
        source_id=DESC_POLICY.source_id,
        uri=DESC_POLICY.uri,
        content_digest=DESC_POLICY.content_digest,
        text="Logs are retained for a year.",
        retrieved_at=DESC_POLICY.retrieved_at,
    )
    changed = EvidenceBundle.build(original.bundle_id, original.claims, (drifted, DESC_RUNBOOK))
    ok, failures = verify(changed, original.claims)
    assert not ok
    assert any("digest drift" in item for item in failures)
