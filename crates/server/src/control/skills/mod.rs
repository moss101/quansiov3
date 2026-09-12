//! The Skill control plane (INT-009, DOMAIN.md §11.5).
//!
//! A skill version is promoted along a fixed ladder, and only its last rung is production:
//! `DRAFT → CANDIDATE → EVALUATING → APPROVED → ACTIVE → DEPRECATED → RETIRED`, with
//! `EVALUATING → REJECTED`. The state machine lives here, in the control plane, because promotion
//! is an administrative decision about an asset — not something a model, a task or a skill may do
//! for itself. The resolver (`python/intelligence/skills`) consumes the view this store exposes and
//! sees only `ACTIVE` versions.

pub mod state;
pub mod store;

use thiserror::Error;

pub use state::{promote, SkillStateError, SkillStatus};
pub use store::{
    NewSkill, NewSkillVersion, Promotion, Skill, SkillAdmin, SkillScope, SkillStore, SkillVersion,
};

/// Repository path of this module's canonical owner.
pub const SKILLS_OWNER: &str = "crates/server/src/control/skills";

/// A skill control-plane refusal.
#[derive(Debug, Error)]
pub enum SkillControlError {
    /// A version was promoted along an edge the §11.5 ladder does not have.
    #[error(transparent)]
    State(#[from] SkillStateError),
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store refused the transaction that carried the mutation.
    #[error(transparent)]
    Event(#[from] quansio_events::EventError),
    /// The skill is not registered in this tenant.
    #[error("skill {skill_id} not found")]
    SkillNotFound {
        /// Requested skill.
        skill_id: String,
    },
    /// The skill version is not registered in this tenant.
    #[error("skill version {version_id} not found")]
    VersionNotFound {
        /// Requested version.
        version_id: String,
    },
    /// A second version cannot be ACTIVE at the same time.
    #[error(
        "skill {skill_id} already has ACTIVE version {active_version_id}; \
             deprecate it before promoting {version_id}"
    )]
    AnotherVersionIsActive {
        /// The skill.
        skill_id: String,
        /// The version currently in production.
        active_version_id: String,
        /// The version that was refused.
        version_id: String,
    },
    /// A canonical identity could not be read from a row.
    #[error("malformed skill row: {0}")]
    Malformed(String),
}

impl SkillControlError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::State(error) => error.code(),
            Self::Database(_) | Self::Event(_) | Self::Malformed(_) => "INTERNAL",
            Self::SkillNotFound { .. } | Self::VersionNotFound { .. } => "NOT_FOUND",
            Self::AnotherVersionIsActive { .. } => "CONFLICT_STATE",
        }
    }
}

impl From<quansio_core::CoreError> for SkillControlError {
    fn from(error: quansio_core::CoreError) -> Self {
        Self::Malformed(error.to_string())
    }
}
