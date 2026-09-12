//! Content-trust labelling and policy tier escalation (DOMAIN.md §12, INT-012).
//!
//! Trust labelling itself belongs to `ContextProjection`; this module is the *policy
//! enforcement* half of DOSSIER.md §5's "content trust labelling" row. A proposal
//! records `derived_from_trust` = the most-untrusted label among the segments its causal
//! window referenced. Until INT-012 ships the labelling source, the value is an input
//! with a documented, fail-closed default: an unlabelled or absent derivation is treated
//! as [`TrustLevel::UntrustedExternal`], so escalation can only make policy stricter.

use core::fmt;
use core::str::FromStr;

use quansio_capability::Tier;
use serde::{Deserialize, Serialize};

/// The canonical content-trust levels (DOMAIN.md §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TrustLevel {
    /// Runtime-rendered system, policy and tool definitions.
    #[serde(rename = "TRUSTED_SYSTEM")]
    TrustedSystem,
    /// Messages authored by authenticated workspace members.
    #[serde(rename = "TRUSTED_USER")]
    TrustedUser,
    /// An `ACTIVE` KnowledgeEntry or Skill.
    #[serde(rename = "VERIFIED_KNOWLEDGE")]
    VerifiedKnowledge,
    /// Prior assistant or worker output.
    #[serde(rename = "AGENT_GENERATED")]
    AgentGenerated,
    /// Web pages, fetched documents, emails, connector payloads, tool output, uploads.
    #[serde(rename = "UNTRUSTED_EXTERNAL")]
    UntrustedExternal,
}

impl TrustLevel {
    /// Every level, most trusted first.
    pub const ALL: [Self; 5] = [
        Self::TrustedSystem,
        Self::TrustedUser,
        Self::VerifiedKnowledge,
        Self::AgentGenerated,
        Self::UntrustedExternal,
    ];

    /// The canonical uppercase wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrustedSystem => "TRUSTED_SYSTEM",
            Self::TrustedUser => "TRUSTED_USER",
            Self::VerifiedKnowledge => "VERIFIED_KNOWLEDGE",
            Self::AgentGenerated => "AGENT_GENERATED",
            Self::UntrustedExternal => "UNTRUSTED_EXTERNAL",
        }
    }

    /// Parse a canonical level name.
    ///
    /// # Errors
    /// Returns the rejected value when it is not a canonical trust level; the caller
    /// resolves this fail-closed rather than assuming a trusted level.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|level| level.as_str() == value)
            .ok_or_else(|| value.to_string())
    }

    /// Trust rank: `0` most trusted, `4` least trusted.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::TrustedSystem => 0,
            Self::TrustedUser => 1,
            Self::VerifiedKnowledge => 2,
            Self::AgentGenerated => 3,
            Self::UntrustedExternal => 4,
        }
    }

    /// Whether this is the data-only external level.
    #[must_use]
    pub const fn is_untrusted_external(self) -> bool {
        matches!(self, Self::UntrustedExternal)
    }
}

impl fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TrustLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// The fail-closed default for an absent or unlabelled derivation (DOMAIN.md §12).
pub const FAIL_CLOSED_TRUST: TrustLevel = TrustLevel::UntrustedExternal;

/// The INT-012 seam that labels a referenced context segment.
///
/// A segment the source cannot label is treated as [`FAIL_CLOSED_TRUST`], never as
/// trusted: an unavailable labelling service must not widen authority.
pub trait TrustLabellingSource {
    /// The trust level of one context segment, or `None` when it is unlabelled.
    fn trust_of(&self, segment_ref: &str) -> Option<TrustLevel>;
}

/// A source that labels nothing; every derivation fails closed to untrusted.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableTrustLabelling;

impl TrustLabellingSource for UnavailableTrustLabelling {
    fn trust_of(&self, _segment_ref: &str) -> Option<TrustLevel> {
        None
    }
}

/// The most-untrusted label among `segments` (DOMAIN.md §12 rule 2).
///
/// An empty segment set and any unlabelled segment fail closed to
/// [`FAIL_CLOSED_TRUST`]; the result is never more trusted than the evidence allows.
#[must_use]
pub fn derived_from_trust(
    source: &(impl TrustLabellingSource + ?Sized),
    segments: &[String],
) -> TrustLevel {
    if segments.is_empty() {
        return FAIL_CLOSED_TRUST;
    }
    let mut worst = TrustLevel::TrustedSystem;
    for segment in segments {
        match source.trust_of(segment) {
            Some(level) => worst = worst.max(level),
            None => return FAIL_CLOSED_TRUST,
        }
    }
    worst
}

/// The tier used for policy purposes, and how it was reached (DOMAIN.md §7.1, §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escalation {
    /// The catalog tier of the effect class.
    pub catalog_tier: Tier,
    /// The tier policy evaluates against.
    pub effective_tier: Tier,
    /// Whether the untrusted derivation raised the tier.
    pub escalated: bool,
    /// The derivation the escalation was computed from.
    pub trust: TrustLevel,
}

impl Escalation {
    /// Whether policy requires an approval receipt for this proposal (tier ≥ 3).
    #[must_use]
    pub const fn requires_receipt(self) -> bool {
        self.effective_tier.get() >= 3
    }

    /// Whether a `UserRule` may move this proposal to `always` (DOMAIN.md §7.1, §12).
    ///
    /// Tier 4 never accepts `always`, and an escalated proposal never accepts it.
    #[must_use]
    pub const fn allows_always(self) -> bool {
        self.effective_tier.get() < 4 && !self.escalated
    }
}

/// Escalate a proposal one tier when it derives from `UNTRUSTED_EXTERNAL` content.
///
/// DOMAIN.md §12 rule 2 escalates tier ≥ 2 by one tier (capped at tier 4); a stricter
/// reading than escalating only tier ≥ 3, so an untrusted proposal can never be treated
/// as less consequential than the domain model requires.
#[must_use]
pub fn escalate(catalog_tier: Tier, trust: TrustLevel) -> Escalation {
    let escalated = trust.is_untrusted_external() && catalog_tier.get() >= 2;
    let effective_tier = if escalated {
        Tier::new((catalog_tier.get() + 1).min(4)).unwrap_or(catalog_tier)
    } else {
        catalog_tier
    };
    Escalation {
        catalog_tier,
        effective_tier,
        escalated,
        trust,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_trust_level_round_trips() {
        for level in TrustLevel::ALL {
            assert_eq!(TrustLevel::parse(level.as_str()).expect("level"), level);
        }
        assert!(TrustLevel::parse("trusted").is_err());
    }

    #[test]
    fn untrusted_external_escalates_tier_two_and_above_by_one() {
        for raw in 0u8..=4 {
            let tier = Tier::new(raw).expect("tier");
            let result = escalate(tier, TrustLevel::UntrustedExternal);
            if raw >= 2 {
                assert!(result.escalated, "tier {raw}");
                assert_eq!(result.effective_tier.get(), (raw + 1).min(4));
            } else {
                assert!(!result.escalated, "tier {raw}");
                assert_eq!(result.effective_tier, tier);
            }
        }
    }

    #[test]
    fn trusted_derivations_never_escalate() {
        for level in [
            TrustLevel::TrustedSystem,
            TrustLevel::TrustedUser,
            TrustLevel::VerifiedKnowledge,
            TrustLevel::AgentGenerated,
        ] {
            let tier = Tier::new(3).expect("tier");
            let result = escalate(tier, level);
            assert!(!result.escalated);
            assert!(result.allows_always());
        }
    }

    #[test]
    fn escalation_forbids_always_and_requires_receipt() {
        let result = escalate(Tier::new(2).expect("tier"), TrustLevel::UntrustedExternal);
        assert!(result.escalated);
        assert_eq!(result.effective_tier.get(), 3);
        assert!(result.requires_receipt());
        assert!(!result.allows_always());
    }

    #[test]
    fn unavailable_labelling_fails_closed() {
        let source = UnavailableTrustLabelling;
        assert_eq!(
            derived_from_trust(&source, &["seg_a".to_string()]),
            FAIL_CLOSED_TRUST
        );
        assert_eq!(derived_from_trust(&source, &[]), FAIL_CLOSED_TRUST);
    }

    struct Fixed(std::collections::HashMap<String, TrustLevel>);

    impl TrustLabellingSource for Fixed {
        fn trust_of(&self, segment_ref: &str) -> Option<TrustLevel> {
            self.0.get(segment_ref).copied()
        }
    }

    #[test]
    fn derived_trust_is_the_most_untrusted_label() {
        let source = Fixed(std::collections::HashMap::from([
            ("a".to_string(), TrustLevel::TrustedUser),
            ("b".to_string(), TrustLevel::AgentGenerated),
        ]));
        assert_eq!(
            derived_from_trust(&source, &["a".to_string(), "b".to_string()]),
            TrustLevel::AgentGenerated
        );
        assert_eq!(
            derived_from_trust(&source, &["a".to_string()]),
            TrustLevel::TrustedUser
        );
    }
}
