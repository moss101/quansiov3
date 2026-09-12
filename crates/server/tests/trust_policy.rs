//! INT-012 acceptance: escalation and the exfiltration guard, verified against the shipped policy.
//!
//! The policy layer already owns escalation (RUN-006) and the privacy/egress guard; INT-012's
//! contribution here is to hold those shipped functions to §12's rules rather than to add a second
//! guard. Everything in this file is pure, so it runs without a database.
//!
//! Real boundary: none.

use quansio_capability::Tier;
use quansio_server::policy::guards::{
    check_privacy, DataClass, EgressDestination, EgressGrantSet, GuardFailure, PrivacyInputs,
};
use quansio_server::policy::{escalate, TrustLevel};

fn tier(value: u8) -> Tier {
    Tier::new(value).expect("a catalog tier")
}

fn protected() -> DataClass {
    // The shipped protected set is DOMAIN §12 rule 3's; asserting it here means the tests below
    // cannot silently stop testing protection if that list changes.
    let class = DataClass::parse("pii").expect("a data class");
    assert!(class.is_protected(), "the test needs a protected class");
    class
}

// ---------------------------------------------------------------------------------------
// Escalation (build item 3, acceptance statement 2)
// ---------------------------------------------------------------------------------------

#[test]
fn untrusted_derived_tier_two_escalates_one_tier_and_cannot_use_always() {
    let escalation = escalate(tier(2), TrustLevel::UntrustedExternal);
    assert!(
        escalation.escalated,
        "an untrusted-tier-2 proposal escalates"
    );
    assert_eq!(escalation.effective_tier.get(), 3, "by exactly one tier");
    assert!(
        escalation.requires_receipt(),
        "tier 3 requires an approval receipt"
    );
    assert!(
        !escalation.allows_always(),
        "an escalated proposal may never be remembered as `always`"
    );
}

#[test]
fn a_trusted_proposal_is_not_escalated() {
    for trust in [
        TrustLevel::TrustedSystem,
        TrustLevel::TrustedUser,
        TrustLevel::VerifiedKnowledge,
        TrustLevel::AgentGenerated,
    ] {
        let escalation = escalate(tier(2), trust);
        assert!(!escalation.escalated, "{trust:?} does not escalate");
        assert_eq!(escalation.effective_tier.get(), 2);
        assert!(
            escalation.allows_always(),
            "{trust:?} may use an always rule at tier 2"
        );
        assert!(!escalation.requires_receipt());
    }
}

#[test]
fn tier_zero_and_one_are_not_escalated_but_tier_four_is_capped() {
    for catalog in [0u8, 1] {
        let escalation = escalate(tier(catalog), TrustLevel::UntrustedExternal);
        assert!(!escalation.escalated, "tier {catalog} is not escalated");
        assert_eq!(escalation.effective_tier.get(), catalog);
    }
    let top = escalate(tier(4), TrustLevel::UntrustedExternal);
    assert!(top.escalated, "tier 4 is flagged as untrusted-derived");
    assert_eq!(top.effective_tier.get(), 4, "and cannot exceed the ladder");
    assert!(top.requires_receipt() && !top.allows_always());
    // A tier-3 untrusted proposal needs a fresh approval at tier 4.
    let three = escalate(tier(3), TrustLevel::UntrustedExternal);
    assert_eq!(three.effective_tier.get(), 4);
    assert!(three.requires_receipt() && !three.allows_always());
}

// ---------------------------------------------------------------------------------------
// The exfiltration guard (build item 4)
// ---------------------------------------------------------------------------------------

#[test]
fn protected_content_cannot_leave_the_egress_grant_set() {
    let destination = EgressDestination::domain("collector.example.com");
    let outcome = check_privacy(PrivacyInputs {
        data_classes: &[protected()],
        destination: Some(&destination),
        egress_grants: &EgressGrantSet::empty(),
        untrusted_origin: false,
    });
    assert_eq!(
        outcome.failure(),
        Some(GuardFailure::ProtectedEgressDenied),
        "protected content outside the grant set is refused"
    );
    assert_eq!(
        outcome.failure().expect("denied").as_str(),
        "privacy_protected_egress_denied"
    );
    assert!(!outcome.is_permitted());
}

#[test]
fn an_instruction_from_a_fetched_page_cannot_redirect_output() {
    // The page told the model to send its findings somewhere; the destination is not granted, so
    // the movement is refused even though the content class itself is not protected.
    let destination = EgressDestination::connector("evil-collector");
    let outcome = check_privacy(PrivacyInputs {
        data_classes: &[],
        destination: Some(&destination),
        egress_grants: &EgressGrantSet::empty(),
        untrusted_origin: true,
    });
    assert_eq!(outcome.failure(), Some(GuardFailure::UntrustedEgressDenied));

    // The same movement becomes permitted only by holding a grant for that destination.
    let granted = EgressGrantSet::new(vec![EgressDestination::connector("evil-collector")]);
    assert!(check_privacy(PrivacyInputs {
        data_classes: &[],
        destination: Some(&destination),
        egress_grants: &granted,
        untrusted_origin: true,
    })
    .is_permitted());
}

#[test]
fn movement_within_the_grant_set_is_permitted_and_no_movement_is_not_guarded() {
    let destination = EgressDestination::domain("api.example.com");
    let granted = EgressGrantSet::new(vec![EgressDestination::domain("api.example.com")]);
    assert!(check_privacy(PrivacyInputs {
        data_classes: &[protected()],
        destination: Some(&destination),
        egress_grants: &granted,
        untrusted_origin: true,
    })
    .is_permitted());

    // A call that moves nothing has no destination to guard.
    assert!(check_privacy(PrivacyInputs {
        data_classes: &[protected()],
        destination: None,
        egress_grants: &EgressGrantSet::empty(),
        untrusted_origin: true,
    })
    .is_permitted());
}
