//! Skill recording (ADP-003, DOMAIN.md §11.7): a person demonstrates a workflow once and it becomes a
//! reviewable `DRAFT` skill.
//!
//! This module owns the recorder *session*: who is recording ([`store::RecordingStore`] and its
//! `skill_recordings` row), what may enter ([`archive`] and [`redact`]), what may come out
//! ([`proposal`], which validates whatever the intelligence plane proposes) and how a proposal reaches the
//! Skill Registry ([`compile`]). The archive's bytes are an Artifact, the skill is a registry row; the
//! plane proposes and this module commits.
//!
//! Three rules shape everything here:
//!
//! * a recording finishes only through the person's own command, and the owner fence (user, session
//!   family, epoch) is checked on every call;
//! * nothing is stored or sent that redaction would still remove;
//! * a proposal is untrusted data: it can name only tools and effect classes the registry has, never
//!   coordinates, and it is `DRAFT` and nothing more.

pub mod archive;
pub mod compile;
pub mod config;
pub mod proposal;
pub mod redact;
pub mod store;

use thiserror::Error;

pub use archive::{ArchiveReport, Expected, ARCHIVE_MEDIA_TYPE};
pub use compile::{CompileOutcome, RecordingCompiler, SubmitOutcome, SlotDecision};
pub use config::{ConfigError, RecordingConfig};
pub use redact::{Category, Counts, RULE_SET};
pub use store::{Recording, RecordingCaller, RecordingStatus, RecordingStore};

/// Repository path of this module's canonical owner.
pub const RECORDING_OWNER: &str = "crates/server/src/control/skills/recording";

/// A refused recording operation.
#[derive(Debug, Error)]
pub enum RecordingError {
    /// A database failure.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// The RuntimeEvent store refused the transaction that carried the mutation.
    #[error(transparent)]
    Event(#[from] quansio_events::EventError),
    /// The artifact store refused.
    #[error("artifact store: {0}")]
    Artifact(String),
    /// The Skill Registry refused.
    #[error(transparent)]
    Skill(#[from] super::SkillControlError),
    /// The intelligence plane refused or could not be reached.
    #[error(transparent)]
    Plane(#[from] crate::intelligence::IntelligenceError),
    /// There is no such recording for this person.
    #[error("recording {id} not found")]
    NotFound {
        /// The requested recording.
        id: String,
    },
    /// The call's user, session family or epoch is not the recording's.
    #[error("the recording belongs to another owner: {reason}")]
    OwnerChanged {
        /// Which part of the fence failed.
        reason: &'static str,
    },
    /// Compile or submit before the person finished.
    #[error("recording {id} has not been finished by the person")]
    NotFinished {
        /// The recording.
        id: String,
    },
    /// The recording is not in a state the call is allowed from.
    #[error("recording {id} is {status}: {detail}")]
    State {
        /// The recording.
        id: String,
        /// Its status.
        status: String,
        /// What the call needed.
        detail: String,
    },
    /// A repeated start key came with other arguments.
    #[error("start_key {key:?} was used with other arguments")]
    IdempotencyMismatch {
        /// The key.
        key: String,
    },
    /// The archive is malformed.
    #[error("invalid archive: {0}")]
    InvalidArchive(String),
    /// The archive is over a configured limit.
    #[error("archive over a limit: {0}")]
    ArchiveBounds(String),
    /// Redaction would still remove something from the archive.
    #[error("the archive still holds material redaction removes ({categories:?} at {pointers:?})")]
    Unredacted {
        /// The categories found.
        categories: Vec<String>,
        /// Where, as JSON pointers. Never the matched text.
        pointers: Vec<String>,
    },
    /// The plane's proposal broke a rule.
    #[error("proposal rejected by {rule}: {detail}")]
    ProposalRejected {
        /// The rule.
        rule: &'static str,
        /// What was found.
        detail: String,
    },
    /// A request field is missing or malformed.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The configuration cannot be trusted.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A stored row could not be read back.
    #[error("malformed recording row: {0}")]
    Malformed(String),
}

impl RecordingError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_)
            | Self::Event(_)
            | Self::Artifact(_)
            | Self::Config(_)
            | Self::Malformed(_) => "INTERNAL",
            Self::Skill(error) => error.code(),
            Self::Plane(crate::intelligence::IntelligenceError::Refused { code, .. })
                if code == "VALIDATION_SCHEMA" =>
            {
                "VALIDATION_SCHEMA"
            }
            Self::Plane(_) => "TARGET_UNAVAILABLE",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::OwnerChanged { .. } => "RECORDING_OWNER_CHANGED",
            Self::NotFinished { .. } => "RECORDING_NOT_FINISHED",
            Self::State { .. } => "CONFLICT_STATE",
            Self::IdempotencyMismatch { .. } => "CONFLICT_IDEMPOTENCY_MISMATCH",
            Self::InvalidArchive(_) | Self::Invalid(_) => "VALIDATION_SCHEMA",
            Self::ArchiveBounds(_) => "VALIDATION_BOUNDS",
            Self::Unredacted { .. } => "RECORDING_UNREDACTED",
            Self::ProposalRejected { .. } => "RECORDING_PROPOSAL_REJECTED",
        }
    }

    /// Structured details for the public error: categories and pointers, never a value.
    #[must_use]
    pub fn details(&self) -> Option<serde_json::Value> {
        match self {
            Self::Unredacted { categories, pointers } => {
                Some(serde_json::json!({ "categories": categories, "pointers": pointers }))
            }
            Self::ProposalRejected { rule, .. } => Some(serde_json::json!({ "rule": rule })),
            Self::OwnerChanged { reason } => Some(serde_json::json!({ "reason": reason })),
            _ => None,
        }
    }
}

impl From<crate::artifacts::ArtifactError> for RecordingError {
    fn from(error: crate::artifacts::ArtifactError) -> Self {
        Self::Artifact(error.to_string())
    }
}

impl From<quansio_core::CoreError> for RecordingError {
    fn from(error: quansio_core::CoreError) -> Self {
        Self::Malformed(error.to_string())
    }
}
