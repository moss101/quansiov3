//! The Skill control plane (INT-009, DOMAIN.md §11.5).
//!
//! A skill version is promoted along a fixed ladder, and only its last rung is production:
//! `DRAFT → CANDIDATE → EVALUATING → APPROVED → ACTIVE → DEPRECATED → RETIRED`, with
//! `EVALUATING → REJECTED`. The state machine lives here, in the control plane, because promotion
//! is an administrative decision about an asset — not something a model, a task or a skill may do
//! for itself. The resolver (`python/intelligence/skills`) consumes the view this store exposes and
//! sees only `ACTIVE` versions.

pub mod state;

use thiserror::Error;

pub use state::{promote, SkillStateError, SkillStatus};

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
}

impl SkillControlError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::State(error) => error.code(),
            Self::Database(_) => "INTERNAL",
        }
    }
}
