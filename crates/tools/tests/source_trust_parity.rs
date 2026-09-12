//! The Tool contract's `source_trust` ladder must match the generated contract.
//!
//! `crates/tools` keeps its own copy of DOMAIN.md §12's trust levels so the crate stays
//! free of the protobuf bindings; this test is the drift gate.

use quansio_contracts::generated::trust::TrustLevel;
use quansio_tools::SourceTrust;

fn contract_level(level: SourceTrust) -> TrustLevel {
    match level {
        SourceTrust::TrustedSystem => TrustLevel::TrustedSystem,
        SourceTrust::TrustedUser => TrustLevel::TrustedUser,
        SourceTrust::VerifiedKnowledge => TrustLevel::VerifiedKnowledge,
        SourceTrust::AgentGenerated => TrustLevel::AgentGenerated,
        SourceTrust::UntrustedExternal => TrustLevel::UntrustedExternal,
    }
}

#[test]
fn source_trust_matches_the_generated_contract() {
    assert_eq!(
        SourceTrust::ALL.len(),
        5,
        "DOMAIN.md §12 names five trust levels"
    );
    for level in SourceTrust::ALL {
        let contract = contract_level(level);
        assert_eq!(
            level.contract_name(),
            contract.as_str_name(),
            "{level:?} must use the generated contract spelling"
        );
        assert_eq!(
            TrustLevel::from_str_name(level.contract_name()),
            Some(contract),
            "{level:?} must round-trip through the generated contract"
        );
    }
}

#[test]
fn only_untrusted_external_is_data_only() {
    for level in SourceTrust::ALL {
        assert_eq!(
            level.is_data_only(),
            level == SourceTrust::UntrustedExternal,
            "{level:?} data-only status is wrong"
        );
    }
}
