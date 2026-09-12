//! Data-class privacy guards and sequence guards (DOMAIN.md §7.1, §12 rule 3).
//!
//! The privacy guard refuses to move a protected data class to a destination that is
//! not already inside the egress grant set, and refuses to move `UNTRUSTED_EXTERNAL`
//! content to an ungranted destination. Both are deny-only: an empty grant set denies,
//! an unparseable data class denies, and a destination that is not explicitly covered
//! denies.
//!
//! Sequence guards encode ordering requirements over durable facts — an approval cannot
//! be used before it is granted, and a settlement cannot precede its reservation. The
//! guard set is evaluated against [`SequenceContext`], which the caller builds from the
//! Effect Ledger and approval store; a fact the context does not carry is treated as
//! absent, which fails closed.

use core::fmt;

use serde::{Deserialize, Serialize};

/// Data classes used by rules and privacy constraints (DOMAIN.md §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DataClass(String);

impl DataClass {
    /// Canonical protected data classes: moving these outside the egress grant set is a
    /// tier-4 `data.upload.protected` effect (DOMAIN.md §7.1).
    pub const PROTECTED: [&'static str; 8] = [
        "pii",
        "phi",
        "financial",
        "payment",
        "credential",
        "secret",
        "protected",
        "confidential",
    ];

    /// Validate and build a data class.
    ///
    /// # Errors
    /// Returns the rejected value when it is not a lowercase dotted identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'.'
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(value)
        }
    }

    /// The canonical string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this class is protected from ungranted egress.
    #[must_use]
    pub fn is_protected(&self) -> bool {
        Self::PROTECTED.contains(&self.0.as_str())
    }
}

impl fmt::Display for DataClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A destination data could move to (DOMAIN.md §12 rule 3 egress grant set).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EgressDestination {
    /// Destination kind (`domain`, `connector`, `recipient`, `webhook`).
    pub kind: String,
    /// Destination value (`api.example.com`, `cnx_…`, an address, …).
    pub value: String,
}

impl EgressDestination {
    /// Build a destination.
    #[must_use]
    pub fn new(kind: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            value: value.into(),
        }
    }

    /// A host destination.
    #[must_use]
    pub fn domain(value: impl Into<String>) -> Self {
        Self::new("domain", value)
    }

    /// A connector destination.
    #[must_use]
    pub fn connector(value: impl Into<String>) -> Self {
        Self::new("connector", value)
    }
}

/// The set of destinations an actor is already granted egress to (DOMAIN.md §6.1, §12).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressGrantSet {
    destinations: Vec<EgressDestination>,
}

impl EgressGrantSet {
    /// Build the grant set from resolved egress destinations.
    #[must_use]
    pub fn new(destinations: Vec<EgressDestination>) -> Self {
        Self { destinations }
    }

    /// An empty grant set; every protected movement is denied against it.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            destinations: Vec::new(),
        }
    }

    /// The granted destinations.
    #[must_use]
    pub fn destinations(&self) -> &[EgressDestination] {
        &self.destinations
    }

    /// Whether `destination` is inside the grant set.
    ///
    /// `*` covers every value of the same kind; matching is exact otherwise, so an
    /// unknown destination is not granted implicitly.
    #[must_use]
    pub fn covers(&self, destination: &EgressDestination) -> bool {
        self.destinations.iter().any(|granted| {
            granted.kind == destination.kind
                && (granted.value == "*" || granted.value == destination.value)
        })
    }
}

/// A sequence requirement checked against durable facts (DOMAIN.md §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceGuard {
    /// Every approval used must first have been granted.
    ApprovalGrantedBeforeUse,
    /// Every settled effect must first have been reserved.
    ReservationBeforeSettlement,
}

impl SequenceGuard {
    /// Every guard.
    pub const ALL: [Self; 2] = [
        Self::ApprovalGrantedBeforeUse,
        Self::ReservationBeforeSettlement,
    ];

    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalGrantedBeforeUse => "approval_granted_before_use",
            Self::ReservationBeforeSettlement => "reservation_before_settlement",
        }
    }

    /// Whether the guard holds for `context`.
    #[must_use]
    pub fn holds(self, context: &SequenceContext) -> bool {
        match self {
            Self::ApprovalGrantedBeforeUse => context
                .used_approval_ids
                .iter()
                .all(|id| context.granted_approval_ids.contains(id)),
            Self::ReservationBeforeSettlement => context
                .settled_effect_ids
                .iter()
                .all(|id| context.reserved_effect_ids.contains(id)),
        }
    }
}

impl fmt::Display for SequenceGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Durable facts a sequence guard is checked against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SequenceContext {
    /// Approvals whose receipt has been granted.
    pub granted_approval_ids: Vec<String>,
    /// Approvals a proposal would use.
    pub used_approval_ids: Vec<String>,
    /// Effects with a durable reservation.
    pub reserved_effect_ids: Vec<String>,
    /// Effects the proposal would settle.
    pub settled_effect_ids: Vec<String>,
}

impl SequenceContext {
    /// An empty context: every fact is absent, so every guard over it fails closed.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            granted_approval_ids: Vec::new(),
            used_approval_ids: Vec::new(),
            reserved_effect_ids: Vec::new(),
            settled_effect_ids: Vec::new(),
        }
    }
}

/// The result of a privacy or sequence guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardOutcome {
    /// The guard permits the proposal.
    Permitted,
    /// The guard denies the proposal.
    Denied(GuardFailure),
}

impl GuardOutcome {
    /// Whether the guard permitted the proposal.
    #[must_use]
    pub const fn is_permitted(self) -> bool {
        matches!(self, Self::Permitted)
    }

    /// The typed failure, when the guard denied.
    #[must_use]
    pub const fn failure(self) -> Option<GuardFailure> {
        match self {
            Self::Permitted => None,
            Self::Denied(failure) => Some(failure),
        }
    }
}

/// Why a privacy or sequence guard denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardFailure {
    /// A protected data class would move to a destination outside the egress grant set.
    ProtectedEgressDenied,
    /// `UNTRUSTED_EXTERNAL` content would move to an ungranted destination.
    UntrustedEgressDenied,
    /// A sequence guard did not hold.
    SequenceViolation(SequenceGuard),
}

impl GuardFailure {
    /// The canonical reason string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProtectedEgressDenied => "privacy_protected_egress_denied",
            Self::UntrustedEgressDenied => "privacy_untrusted_egress_denied",
            Self::SequenceViolation(SequenceGuard::ApprovalGrantedBeforeUse) => {
                "sequence_approval_not_granted"
            }
            Self::SequenceViolation(SequenceGuard::ReservationBeforeSettlement) => {
                "sequence_settlement_before_reservation"
            }
        }
    }
}

impl fmt::Display for GuardFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The privacy/egress guard input.
#[derive(Debug, Clone, Copy)]
pub struct PrivacyInputs<'a> {
    /// Data classes carried by the proposal.
    pub data_classes: &'a [DataClass],
    /// Destination the data would move to, when the effect moves data.
    pub destination: Option<&'a EgressDestination>,
    /// Destinations the actor is already granted egress to.
    pub egress_grants: &'a EgressGrantSet,
    /// Whether the proposal derives from `UNTRUSTED_EXTERNAL` content.
    pub untrusted_origin: bool,
}

/// Deny protected-class or untrusted data movement outside the egress grant set.
#[must_use]
pub fn check_privacy(inputs: PrivacyInputs<'_>) -> GuardOutcome {
    let Some(destination) = inputs.destination else {
        // No movement: nothing to guard.
        return GuardOutcome::Permitted;
    };
    if inputs.egress_grants.covers(destination) {
        return GuardOutcome::Permitted;
    }
    if inputs.data_classes.iter().any(DataClass::is_protected) {
        return GuardOutcome::Denied(GuardFailure::ProtectedEgressDenied);
    }
    if inputs.untrusted_origin {
        return GuardOutcome::Denied(GuardFailure::UntrustedEgressDenied);
    }
    GuardOutcome::Permitted
}

/// Deny a proposal whose sequence guards do not hold.
#[must_use]
pub fn check_sequence(guards: &[SequenceGuard], context: &SequenceContext) -> GuardOutcome {
    for guard in guards {
        if !guard.holds(context) {
            return GuardOutcome::Denied(GuardFailure::SequenceViolation(*guard));
        }
    }
    GuardOutcome::Permitted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(value: &str) -> DataClass {
        DataClass::parse(value).expect("data class")
    }

    #[test]
    fn protected_data_class_names_are_protected() {
        for name in DataClass::PROTECTED {
            assert!(class(name).is_protected(), "{name}");
        }
        assert!(!class("public").is_protected());
        assert!(DataClass::parse("PII").is_err());
    }

    #[test]
    fn protected_movement_without_egress_grant_is_denied() {
        let classes = vec![class("pii")];
        let destination = EgressDestination::domain("api.example.com");
        let grants = EgressGrantSet::default();
        assert_eq!(
            check_privacy(PrivacyInputs {
                data_classes: &classes,
                destination: Some(&destination),
                egress_grants: &grants,
                untrusted_origin: false,
            }),
            GuardOutcome::Denied(GuardFailure::ProtectedEgressDenied)
        );

        let granted = EgressGrantSet::new(vec![EgressDestination::domain("api.example.com")]);
        assert!(check_privacy(PrivacyInputs {
            data_classes: &classes,
            destination: Some(&destination),
            egress_grants: &granted,
            untrusted_origin: false,
        })
        .is_permitted());
    }

    #[test]
    fn untrusted_origin_without_egress_grant_is_denied() {
        assert_eq!(
            check_privacy(PrivacyInputs {
                data_classes: &[],
                destination: Some(&EgressDestination::connector("cnx_x/chan")),
                egress_grants: &EgressGrantSet::default(),
                untrusted_origin: true,
            }),
            GuardOutcome::Denied(GuardFailure::UntrustedEgressDenied)
        );
        // Public data with a trusted origin may move.
        assert!(check_privacy(PrivacyInputs {
            data_classes: &[],
            destination: Some(&EgressDestination::connector("cnx_x/chan")),
            egress_grants: &EgressGrantSet::default(),
            untrusted_origin: false,
        })
        .is_permitted());
        // No destination means no movement to guard.
        assert!(check_privacy(PrivacyInputs {
            data_classes: &[class("pii")],
            destination: None,
            egress_grants: &EgressGrantSet::default(),
            untrusted_origin: true,
        })
        .is_permitted());
    }

    #[test]
    fn approval_cannot_be_used_before_it_is_granted() {
        let context = SequenceContext {
            granted_approval_ids: vec!["apr_a".to_string()],
            used_approval_ids: vec!["apr_b".to_string()],
            ..SequenceContext::default()
        };
        assert_eq!(
            check_sequence(&[SequenceGuard::ApprovalGrantedBeforeUse], &context),
            GuardOutcome::Denied(GuardFailure::SequenceViolation(
                SequenceGuard::ApprovalGrantedBeforeUse
            ))
        );
        let granted = SequenceContext {
            granted_approval_ids: vec!["apr_b".to_string()],
            used_approval_ids: vec!["apr_b".to_string()],
            ..SequenceContext::default()
        };
        assert_eq!(
            check_sequence(&[SequenceGuard::ApprovalGrantedBeforeUse], &granted),
            GuardOutcome::Permitted
        );
    }

    #[test]
    fn settlement_cannot_precede_its_reservation() {
        let context = SequenceContext {
            reserved_effect_ids: Vec::new(),
            settled_effect_ids: vec!["eff_a".to_string()],
            ..SequenceContext::default()
        };
        assert_eq!(
            check_sequence(&[SequenceGuard::ReservationBeforeSettlement], &context),
            GuardOutcome::Denied(GuardFailure::SequenceViolation(
                SequenceGuard::ReservationBeforeSettlement
            ))
        );
        let reserved = SequenceContext {
            reserved_effect_ids: vec!["eff_a".to_string()],
            settled_effect_ids: vec!["eff_a".to_string()],
            ..SequenceContext::default()
        };
        assert_eq!(
            check_sequence(&[SequenceGuard::ReservationBeforeSettlement], &reserved),
            GuardOutcome::Permitted
        );
    }
}
