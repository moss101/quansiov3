//! The context bridge: the runtime's side of the ContextProjection boundary (INT-005, DOMAIN.md §11.2).
//!
//! The intelligence plane owns how context is searched, ranked and packed (`python/intelligence/context`).
//! This module owns what the *runtime* must guarantee about crossing that boundary:
//!
//! 1. **The seam.** [`ContextBridgePort`] is the only way a run asks for a projection, so the
//!    composition root can implement it over INT-001's typed RPC without the runtime knowing how the
//!    plane is reached.
//! 2. **A fail-closed boundary check.** What comes back is not trusted blindly: every segment must
//!    carry a trust level from DOMAIN.md §12, and the projection must name itself and the snapshot it
//!    was built from. An unlabelled or unknown-labelled segment is refused, so a plane bug can never
//!    turn into model-visible content with no provenance.
//! 3. **Durable identity.** A validated projection's id is what a turn records
//!    (`turns.context_projection_id`), so a turn can always be traced to the context it was given.
//!
//! The bridge deliberately does not re-implement search, ranking or packing: that would be a second
//! authority for the plane's job.

pub mod port;

use thiserror::Error;

pub use port::{
    validate_projection, ContextBridgePort, ProjectionRequest, ProjectionSegment,
    ReceivedProjection, UnavailableContextBridge,
};

/// Repository path of this module's canonical owner.
pub const CONTEXT_BRIDGE_OWNER: &str = "crates/server/src/runtime/context_bridge";

/// A context-bridge refusal.
#[derive(Debug, Error)]
pub enum ContextBridgeError {
    /// A segment arrived without a trust level, so its provenance is unknown.
    #[error("segment {segment_id} carries no trust level; a projection segment must have one")]
    UnlabelledSegment {
        /// The offending segment.
        segment_id: String,
    },
    /// A segment named a trust level outside DOMAIN.md §12.
    #[error("segment {segment_id} names an unknown trust level {trust_level:?}")]
    UnknownTrustLevel {
        /// The offending segment.
        segment_id: String,
        /// The unrecognised label.
        trust_level: String,
    },
    /// The projection did not identify itself or its snapshot.
    #[error("the projection is malformed: {detail}")]
    MalformedProjection {
        /// What is missing.
        detail: String,
    },
    /// A stale snapshot: the projection no longer matches the sources.
    #[error("the projection was built from snapshot {built} but the sources are at {current}")]
    StaleSnapshot {
        /// Snapshot the projection was built from.
        built: String,
        /// Snapshot the runtime observes now.
        current: String,
    },
    /// The bridge is not wired yet; the named task owns it.
    #[error("the context bridge is not available: {detail}; owned by {owner}")]
    Unavailable {
        /// Owning task.
        owner: &'static str,
        /// What is missing.
        detail: String,
    },
}

impl ContextBridgeError {
    /// The DOMAIN.md §15 error code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnlabelledSegment { .. }
            | Self::UnknownTrustLevel { .. }
            | Self::MalformedProjection { .. } => "VALIDATION_SCHEMA",
            Self::StaleSnapshot { .. } => "CONFLICT_STATE",
            Self::Unavailable { .. } => "INTERNAL",
        }
    }
}
