"""INT-012 build item 2: an element's trust is the weakest label it references."""

from __future__ import annotations

import pytest

from intelligence.trust import TrustLevel, derive, derive_all
from intelligence.trust.propagation import PropagationError

LABELS = {
    "seg_policy": TrustLevel.TRUSTED_SYSTEM,
    "seg_user": TrustLevel.TRUSTED_USER,
    "seg_page": TrustLevel.UNTRUSTED_EXTERNAL,
    "seg_worker": TrustLevel.AGENT_GENERATED,
}


def test_one_untrusted_reference_makes_the_whole_element_untrusted() -> None:
    """A single page in the chain is enough: an element is only as trusted as its weakest input."""
    propagation = derive(
        element_id="call_1",
        refs=["seg_policy", "seg_page"],
        segment_trust=LABELS,
    )
    assert propagation.trust is TrustLevel.UNTRUSTED_EXTERNAL
    assert propagation.is_untrusted_derived
    assert propagation.referenced == ("seg_page", "seg_policy")


def test_an_element_of_only_trusted_inputs_takes_its_weakest_label() -> None:
    propagation = derive(element_id="call_2", refs=["seg_policy", "seg_user"], segment_trust=LABELS)
    assert propagation.trust is TrustLevel.TRUSTED_USER
    assert not propagation.is_untrusted_derived
    worker = derive(element_id="call_3", refs=["seg_worker", "seg_user"], segment_trust=LABELS)
    assert worker.trust is TrustLevel.AGENT_GENERATED, "agent output is weaker than user input"


def test_an_unknown_reference_fails_closed() -> None:
    """An unknown segment is an unknown origin, not a trusted one."""
    propagation = derive(
        element_id="call_4",
        refs=["seg_policy", "seg_missing"],
        segment_trust=LABELS,
    )
    assert propagation.unknown == ("seg_missing",)
    assert propagation.trust is TrustLevel.UNTRUSTED_EXTERNAL
    assert propagation.is_untrusted_derived


def test_an_element_with_no_references_is_refused() -> None:
    """An unprovable chain must be stated by the caller, not assumed trusted here."""
    with pytest.raises(PropagationError) as raised:
        derive(element_id="call_5", refs=[], segment_trust=LABELS)
    assert raised.value.code == "VALIDATION_SCHEMA"
    assert raised.value.rule_id == "propagation.no_references"


def test_derive_all_is_ordered_total_and_deterministic() -> None:
    elements = {
        "call_a": ["seg_policy"],
        "call_b": ["seg_page"],
        "call_c": ["seg_worker", "seg_user"],
    }
    first = derive_all(elements=elements, segment_trust=LABELS)
    second = derive_all(elements=elements, segment_trust=LABELS)
    assert [p.element_id for p in first] == ["call_a", "call_b", "call_c"]
    assert [p.trust for p in first] == [p.trust for p in second]
    assert [p.is_untrusted_derived for p in first] == [False, True, False]

    # One unprovable element is not skipped: the whole derivation is refused.
    with pytest.raises(PropagationError):
        derive_all(elements={"call_ok": ["seg_policy"], "call_bad": []}, segment_trust=LABELS)
