//! Canonical artifact, evidence and evidence-bundle shapes (DOMAIN.md §10).
//!
//! These types mirror the authoritative PostgreSQL columns exactly; the JSONB
//! `origin`, `retention`, `claims`, `sources` and `verification` columns are the
//! canonical serialization of the structs below. They are plain data so metadata
//! logic can be reasoned about and tested without a database.

use std::fmt;

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, Prefix, TypedId, UlidGenerator};
use serde::{Deserialize, Serialize};

/// Artifact kind (DOMAIN.md §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A document.
    Document,
    /// A spreadsheet.
    Spreadsheet,
    /// A presentation.
    Presentation,
    /// Source code.
    Code,
    /// Structured data.
    Data,
    /// An image.
    Image,
    /// Audio.
    Audio,
    /// Video.
    Video,
    /// An archive.
    Archive,
    /// Anything else.
    Other,
}

impl ArtifactKind {
    /// Canonical wire/database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Spreadsheet => "spreadsheet",
            Self::Presentation => "presentation",
            Self::Code => "code",
            Self::Data => "data",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Archive => "archive",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where an artifact came from (DOMAIN.md §10.1 `origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    /// A conversation message.
    Message,
    /// A run.
    Run,
    /// A routine.
    Routine,
    /// A direct upload.
    Upload,
    /// A connector.
    Connector,
}

impl OriginKind {
    /// Canonical wire/database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Run => "run",
            Self::Routine => "routine",
            Self::Upload => "upload",
            Self::Connector => "connector",
        }
    }
}

/// Artifact origin: the kind plus the identity of the originating object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// What produced the artifact.
    pub kind: OriginKind,
    /// Identity of the producing message, run, routine, upload or connector object.
    #[serde(rename = "ref")]
    pub reference: String,
}

/// Grant level for one principal (DOMAIN.md §10.1 `grants[]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantLevel {
    /// Read the artifact metadata and bytes.
    Read,
    /// Read and add versions.
    Write,
}

impl GrantLevel {
    /// Canonical wire/database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    /// Whether this granted level satisfies a requested level.
    ///
    /// A write grant implies read; a read grant never implies write.
    #[must_use]
    pub const fn satisfies(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Write, _) | (Self::Read, Self::Read)
        )
    }
}

/// One per-principal artifact grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactGrant {
    /// Principal identity (`usr_…` or `sp_…`).
    pub principal: String,
    /// Granted level.
    pub level: GrantLevel,
}

/// Retention class (DOMAIN.md §10.1 `retention.class`).
///
/// `legal_hold` suspends deletion entirely; every other class is time-bounded when
/// `expires_at` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    /// Ordinary retention: deletion is allowed once `expires_at` has passed.
    #[default]
    Standard,
    /// Legal hold: deletion is refused regardless of `expires_at`.
    LegalHold,
}

impl RetentionClass {
    /// Canonical wire/database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::LegalHold => "legal_hold",
        }
    }
}

/// Outcome recorded for one deletion request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionDecision {
    /// The artifact was tombstoned and its bytes removed.
    Tombstoned,
    /// Deletion was refused; the retention metadata explains why.
    Refused,
}

/// Durable record of a deletion decision, so a removal is never silent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionRecord {
    /// What the deletion path decided.
    pub decision: DeletionDecision,
    /// Machine-readable reason (`legal_hold`, `retention_active`, `retention_elapsed`,
    /// `retention_not_set`).
    pub reason: String,
    /// When the decision was recorded.
    pub recorded_at: DateTime<Utc>,
}

/// Retention metadata (DOMAIN.md §10.1 `retention`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retention {
    /// Retention class.
    #[serde(default)]
    pub class: RetentionClass,
    /// Expiry, when the class is time-bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Last recorded deletion decision, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion: Option<DeletionRecord>,
}

impl Retention {
    /// A legal-hold retention that never expires.
    #[must_use]
    pub fn legal_hold() -> Self {
        Self {
            class: RetentionClass::LegalHold,
            expires_at: None,
            deletion: None,
        }
    }

    /// A standard retention that expires at `expires_at`.
    #[must_use]
    pub fn expiring_at(expires_at: DateTime<Utc>) -> Self {
        Self {
            class: RetentionClass::Standard,
            expires_at: Some(expires_at),
            deletion: None,
        }
    }

    /// Whether deletion is blocked by legal hold.
    #[must_use]
    pub const fn is_legal_hold(&self) -> bool {
        matches!(self.class, RetentionClass::LegalHold)
    }

    /// Whether a time-bounded retention window is still open at `now`.
    #[must_use]
    pub fn is_retention_active(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at > now)
    }
}

/// Who produced an artifact version (DOMAIN.md §10.1 `produced_by`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducedBy {
    /// Producing run, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Producing step, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Producing user, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Who captured evidence (DOMAIN.md §10.2 `captured_by`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedBy {
    /// Capturing host kind (`run`, `worker`, `browser`, `user`, …).
    pub kind: String,
    /// Capturing host identity.
    pub id: String,
}

/// Evidence kind (DOMAIN.md §10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Output of a tool call.
    ToolOutput,
    /// A screenshot.
    Screenshot,
    /// A DOM snapshot.
    DomSnapshot,
    /// A log excerpt.
    Log,
    /// A test result.
    TestResult,
    /// An HTTP exchange.
    HttpExchange,
    /// A diff.
    Diff,
    /// A digest-only reference.
    Digest,
    /// An external reference.
    ExternalRef,
}

impl EvidenceKind {
    /// Canonical wire/database value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolOutput => "tool_output",
            Self::Screenshot => "screenshot",
            Self::DomSnapshot => "dom_snapshot",
            Self::Log => "log",
            Self::TestResult => "test_result",
            Self::HttpExchange => "http_exchange",
            Self::Diff => "diff",
            Self::Digest => "digest",
            Self::ExternalRef => "external_ref",
        }
    }
}

/// One claim inside an evidence bundle (DOMAIN.md §10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    /// Stable claim identity.
    pub claim_id: String,
    /// The claimed text span.
    pub text_span: String,
    /// Evidence ids supporting the claim.
    pub evidence_ids: Vec<String>,
    /// Optional support score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_score: Option<f64>,
}

/// One source inside an evidence bundle (DOMAIN.md §10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Stable source identity.
    pub source_id: String,
    /// Source URI, when the source is external.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Artifact reference, when the source is an artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    /// Retrieval timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
    /// Locator of the cited excerpt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt_locator: Option<String>,
    /// Digest of the cited excerpt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt_digest: Option<String>,
}

/// Deterministic and optional semantic verification status (DOMAIN.md §10.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Verification {
    /// Whether deterministic citation checks passed.
    #[serde(default)]
    pub deterministic_passed: bool,
    /// Optional semantic support score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_score: Option<f64>,
    /// When verification ran (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
}

macro_rules! canonical_id {
    ($(#[$meta:meta])* $name:ident, $prefix:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(CanonicalId);

        impl $name {
            /// Generate a fresh identifier (the Rust authority owns identity).
            #[must_use]
            pub fn generate(generator: &mut UlidGenerator) -> Self {
                let id = CanonicalId::new($prefix, generator.generate());
                <Self as TypedId>::from_id(id).expect("prefix is fixed by the type")
            }

            /// Parse a canonical identifier of this entity type.
            ///
            /// # Errors
            /// Returns a [`quansio_core::CoreError`] when the value is not a canonical
            /// id with the expected prefix.
            pub fn parse(value: &str) -> Result<Self, quansio_core::CoreError> {
                <Self as TypedId>::parse(value)
            }

            /// Canonical string form.
            #[must_use]
            pub fn to_canonical(&self) -> String {
                self.0.to_string()
            }
        }

        impl TypedId for $name {
            const PREFIX: Prefix = $prefix;

            fn as_id(&self) -> &CanonicalId {
                &self.0
            }

            fn from_id(id: CanonicalId) -> Result<Self, quansio_core::CoreError> {
                if id.prefix() == Self::PREFIX {
                    Ok(Self(id))
                } else {
                    Err(quansio_core::CoreError::InvalidPrefix {
                        value: id.to_string(),
                        expected: Self::PREFIX.to_string(),
                    })
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = quansio_core::CoreError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                <Self as TypedId>::parse(s)
            }
        }
    };
}

canonical_id!(
    /// Artifact identity (`art_…`, DOMAIN.md §1.1).
    ArtifactId,
    Prefix::Artifact
);
canonical_id!(
    /// Artifact version identity (`artv_…`, DOMAIN.md §1.1).
    ArtifactVersionId,
    Prefix::ArtifactVersion
);
canonical_id!(
    /// Evidence identity (`evd_…`, DOMAIN.md §1.1).
    EvidenceId,
    Prefix::Evidence
);
canonical_id!(
    /// Evidence bundle identity (`evb_…`, DOMAIN.md §1.1).
    EvidenceBundleId,
    Prefix::EvidenceBundle
);
