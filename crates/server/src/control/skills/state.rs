//! The SkillVersion promotion ladder (DOMAIN.md §11.5).

use thiserror::Error;

/// Every state a skill version can hold, in ladder order.
pub const SKILL_STATES: [&str; 8] = [
    "draft",
    "candidate",
    "evaluating",
    "approved",
    "active",
    "deprecated",
    "retired",
    "rejected",
];

/// A refused promotion.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SkillStateError {
    /// The requested state is not on the ladder.
    #[error("{value:?} is not a skill version state")]
    UnknownState {
        /// The unrecognised value.
        value: String,
    },
    /// The ladder has no edge from the current state to the requested one.
    #[error("a skill version cannot move from {from} to {to}")]
    IllegalTransition {
        /// Current state.
        from: String,
        /// Requested state.
        to: String,
    },
}

impl SkillStateError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownState { .. } => "VALIDATION_SCHEMA",
            Self::IllegalTransition { .. } => "RUNTIME_ILLEGAL_TRANSITION",
        }
    }
}

/// The state of one skill version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillStatus {
    /// Authored, not yet submitted for review.
    Draft,
    /// Submitted as a review candidate.
    Candidate,
    /// Under evaluation by its declared eval suite.
    Evaluating,
    /// Review passed; not yet promoted to production.
    Approved,
    /// Promoted: the only state whose versions resolve into production context.
    Active,
    /// Superseded; still visible, no longer resolving.
    Deprecated,
    /// Withdrawn from use.
    Retired,
    /// Evaluation found it unfit.
    Rejected,
}

impl SkillStatus {
    /// Every state, in ladder order.
    pub const ALL: [Self; 8] = [
        Self::Draft,
        Self::Candidate,
        Self::Evaluating,
        Self::Approved,
        Self::Active,
        Self::Deprecated,
        Self::Retired,
        Self::Rejected,
    ];

    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Candidate => "candidate",
            Self::Evaluating => "evaluating",
            Self::Approved => "approved",
            Self::Active => "active",
            Self::Deprecated => "deprecated",
            Self::Retired => "retired",
            Self::Rejected => "rejected",
        }
    }

    /// Parse the durable spelling.
    ///
    /// # Errors
    /// Returns [`SkillStateError::UnknownState`] for a value outside §11.5's ladder.
    pub fn parse(value: &str) -> Result<Self, SkillStateError> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value.trim().to_lowercase())
            .ok_or_else(|| SkillStateError::UnknownState {
                value: value.to_string(),
            })
    }

    /// Whether a version in this state may enter production context.
    ///
    /// Only `ACTIVE` does. `APPROVED` is a review outcome that has not been promoted, so it does
    /// not resolve — the distinction that keeps a review from silently becoming a deployment.
    #[must_use]
    pub const fn resolves(self) -> bool {
        matches!(self, Self::Active)
    }

    /// The states this one may be promoted to.
    #[must_use]
    pub const fn next_states(self) -> &'static [Self] {
        match self {
            Self::Draft => &[Self::Candidate],
            Self::Candidate => &[Self::Evaluating],
            Self::Evaluating => &[Self::Approved, Self::Rejected],
            Self::Approved => &[Self::Active],
            Self::Active => &[Self::Deprecated],
            Self::Deprecated => &[Self::Retired],
            Self::Retired | Self::Rejected => &[],
        }
    }

    /// Whether a promotion from this state to `next` is on the ladder.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        let candidates = self.next_states();
        let mut index = 0;
        while index < candidates.len() {
            if candidates[index] as u8 == next as u8 {
                return true;
            }
            index += 1;
        }
        false
    }
}

/// Move a version along the ladder, refusing an edge the ladder does not have.
///
/// # Errors
/// Returns [`SkillStateError::IllegalTransition`] when the move is not an edge, so an illegal
/// promotion (a draft that jumps straight to active, an already-retired version that is revived)
/// fails closed and names both ends.
pub fn promote(from: SkillStatus, to: SkillStatus) -> Result<SkillStatus, SkillStateError> {
    if from.can_transition_to(to) {
        Ok(to)
    } else {
        Err(SkillStateError::IllegalTransition {
            from: from.as_str().to_string(),
            to: to.as_str().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_is_domain_11_5() {
        let mut walked = vec![SkillStatus::Draft];
        let mut current = SkillStatus::Draft;
        for expected in [
            SkillStatus::Candidate,
            SkillStatus::Evaluating,
            SkillStatus::Approved,
            SkillStatus::Active,
            SkillStatus::Deprecated,
            SkillStatus::Retired,
        ] {
            current = promote(current, expected).expect("a ladder edge");
            walked.push(current);
        }
        assert_eq!(walked.len(), 7);
        assert_eq!(
            SkillStatus::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            SKILL_STATES.to_vec()
        );
        // Evaluating may also reject, and rejected/retired are terminal.
        assert!(SkillStatus::Evaluating.can_transition_to(SkillStatus::Rejected));
        assert!(SkillStatus::Retired.next_states().is_empty());
        assert!(SkillStatus::Rejected.next_states().is_empty());
    }

    #[test]
    fn only_active_resolves() {
        let resolving: Vec<&str> = SkillStatus::ALL
            .into_iter()
            .filter(|status| status.resolves())
            .map(|status| status.as_str())
            .collect();
        assert_eq!(resolving, vec!["active"]);
        assert!(
            !SkillStatus::Approved.resolves(),
            "an approval that was never promoted does not resolve"
        );
    }

    #[test]
    fn an_illegal_promotion_is_refused_and_named() {
        let error =
            promote(SkillStatus::Draft, SkillStatus::Active).expect_err("draft cannot jump");
        assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");
        assert!(matches!(
            error,
            SkillStateError::IllegalTransition { ref from, ref to }
                if from == "draft" && to == "active"
        ));
        // A retired version cannot be revived by a promotion.
        assert!(promote(SkillStatus::Retired, SkillStatus::Active).is_err());
        assert!(promote(SkillStatus::Rejected, SkillStatus::Active).is_err());
        // Leaving the review ladder for production must pass through approved.
        assert!(promote(SkillStatus::Evaluating, SkillStatus::Active).is_err());
    }

    #[test]
    fn an_unknown_state_is_refused() {
        let error = SkillStatus::parse("production").expect_err("not a §11.5 state");
        assert_eq!(error.code(), "VALIDATION_SCHEMA");
        assert_eq!(
            SkillStatus::parse(" ACTIVE ").expect("trimmed"),
            SkillStatus::Active
        );
    }
}
