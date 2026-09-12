//! The bridge seam and the boundary check (INT-005).

use async_trait::async_trait;
use quansio_tools::SourceTrust;

use super::ContextBridgeError;

/// The task that owns the typed RPC boundary the production port crosses.
pub const RPC_BOUNDARY_OWNER: &str = "INT-001";

/// What a run asks the intelligence plane for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRequest {
    /// Run the projection is for.
    pub run_id: String,
    /// Workspace the run belongs to.
    pub workspace_id: String,
    /// The canonical SearchProgram JSON the plane must execute, produced by the plane's own
    /// authority (`python/intelligence/context/search.py`); the runtime carries it opaquely.
    pub program_json: String,
    /// Snapshot of the canonical sources the program must be run against.
    pub snapshot_id: String,
    /// Token budget the projection must respect.
    pub token_budget: u32,
}

/// One segment the plane returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSegment {
    /// Segment identity.
    pub segment_id: String,
    /// Where the content came from.
    pub source: String,
    /// The trust level the plane assigned, as it spells it (DOMAIN.md §12).
    pub trust_level: String,
    /// Tokens the segment costs.
    pub tokens: u32,
}

/// A projection as received, before the boundary check accepts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedProjection {
    /// `ctx_…` projection identity the turn will record.
    pub projection_id: String,
    /// Snapshot the projection was built from.
    pub snapshot_id: String,
    /// Segments, in render order.
    pub segments: Vec<ProjectionSegment>,
    /// Tokens the plane reports as used.
    pub tokens_used: u32,
    /// Whether the plane reports it had to drop context.
    pub degraded: bool,
}

/// A projection the boundary check accepted, with labels resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProjection {
    /// `ctx_…` identity, ready for `TurnInput::context_projection_id`.
    pub projection_id: String,
    /// Snapshot the projection was built from.
    pub snapshot_id: String,
    /// Segments with parsed trust levels.
    pub segments: Vec<(ProjectionSegment, SourceTrust)>,
    /// Tokens used.
    pub tokens_used: u32,
    /// Whether the plane reported degradation.
    pub degraded: bool,
}

impl ValidatedProjection {
    /// The trust level of the least trusted segment, which is what a policy escalates from.
    #[must_use]
    pub fn weakest_trust(&self) -> Option<SourceTrust> {
        self.segments
            .iter()
            .map(|(_, trust)| *trust)
            .max_by_key(|trust| trust.rank())
    }
}

/// Ask the intelligence plane for a run's context projection.
#[async_trait]
pub trait ContextBridgePort: Send + Sync {
    /// Build the projection for a request.
    ///
    /// # Errors
    /// Returns the plane's failure, or [`ContextBridgeError::Unavailable`] when nothing is wired.
    async fn project(
        &self,
        request: ProjectionRequest,
    ) -> Result<ReceivedProjection, ContextBridgeError>;
}

/// The default port: the production one crosses [`RPC_BOUNDARY_OWNER`]'s typed boundary, so an
/// unwired runtime fails closed rather than fabricating context.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableContextBridge;

#[async_trait]
impl ContextBridgePort for UnavailableContextBridge {
    async fn project(
        &self,
        _request: ProjectionRequest,
    ) -> Result<ReceivedProjection, ContextBridgeError> {
        Err(ContextBridgeError::Unavailable {
            owner: RPC_BOUNDARY_OWNER,
            detail: "the context bridge is not wired".to_string(),
        })
    }
}

/// Check what the plane returned before a run may use it.
///
/// This is the runtime's guarantee, not a second implementation of the plane's job: every segment
/// must carry a trust level the §12 ladder recognises, and the projection must name itself and its
/// snapshot. `current_snapshot` refuses a projection the sources have moved past.
///
/// # Errors
/// Returns [`ContextBridgeError::UnlabelledSegment`], [`ContextBridgeError::UnknownTrustLevel`],
/// [`ContextBridgeError::MalformedProjection`] or [`ContextBridgeError::StaleSnapshot`].
pub fn validate_projection(
    received: ReceivedProjection,
    current_snapshot: Option<&str>,
) -> Result<ValidatedProjection, ContextBridgeError> {
    if received.projection_id.trim().is_empty() {
        return Err(ContextBridgeError::MalformedProjection {
            detail: "the projection has no id".to_string(),
        });
    }
    if received.snapshot_id.trim().is_empty() {
        return Err(ContextBridgeError::MalformedProjection {
            detail: "the projection does not name the snapshot it was built from".to_string(),
        });
    }
    if let Some(current) = current_snapshot {
        if current != received.snapshot_id {
            return Err(ContextBridgeError::StaleSnapshot {
                built: received.snapshot_id.clone(),
                current: current.to_string(),
            });
        }
    }
    let mut segments = Vec::with_capacity(received.segments.len());
    for segment in &received.segments {
        if segment.trust_level.trim().is_empty() {
            return Err(ContextBridgeError::UnlabelledSegment {
                segment_id: segment.segment_id.clone(),
            });
        }
        let trust = SourceTrust::parse(&segment.trust_level).map_err(|_| {
            ContextBridgeError::UnknownTrustLevel {
                segment_id: segment.segment_id.clone(),
                trust_level: segment.trust_level.clone(),
            }
        })?;
        segments.push((segment.clone(), trust));
    }
    Ok(ValidatedProjection {
        projection_id: received.projection_id,
        snapshot_id: received.snapshot_id,
        segments,
        tokens_used: received.tokens_used,
        degraded: received.degraded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection(segments: Vec<ProjectionSegment>) -> ReceivedProjection {
        ReceivedProjection {
            projection_id: "ctx_01J8Z3K6F1N8VQ2X5W9Y0TEST".to_string(),
            snapshot_id: "snap_1".to_string(),
            segments,
            tokens_used: 10,
            degraded: false,
        }
    }

    fn segment(id: &str, trust: &str) -> ProjectionSegment {
        ProjectionSegment {
            segment_id: id.to_string(),
            source: "artifact://plan".to_string(),
            trust_level: trust.to_string(),
            tokens: 5,
        }
    }

    #[test]
    fn a_well_formed_projection_is_accepted_with_parsed_labels() {
        let validated = validate_projection(
            projection(vec![
                segment("seg_1", "trusted_system"),
                segment("seg_2", "untrusted_external"),
            ]),
            Some("snap_1"),
        )
        .expect("valid");
        assert_eq!(validated.segments.len(), 2);
        assert_eq!(validated.segments[0].1, SourceTrust::TrustedSystem);
        assert_eq!(
            validated.weakest_trust(),
            Some(SourceTrust::UntrustedExternal),
            "the weakest label is what a policy escalates from"
        );
        assert!(!validated.degraded);
    }

    #[test]
    fn an_unlabelled_segment_is_refused() {
        let error = validate_projection(projection(vec![segment("seg_1", "")]), None)
            .expect_err("a segment with no trust level must be refused");
        assert_eq!(error.code(), "VALIDATION_SCHEMA");
        assert!(matches!(
            error,
            ContextBridgeError::UnlabelledSegment { ref segment_id } if segment_id == "seg_1"
        ));
    }

    #[test]
    fn an_unknown_trust_level_is_refused() {
        for label in ["trusted_vibes", "TRUSTED_SYSTEM", "unspecified"] {
            let error = validate_projection(projection(vec![segment("seg_1", label)]), None)
                .expect_err("an unknown label must be refused");
            assert_eq!(error.code(), "VALIDATION_SCHEMA");
            assert!(
                matches!(error, ContextBridgeError::UnknownTrustLevel { .. }),
                "{label}"
            );
        }
    }

    #[test]
    fn a_projection_without_identity_or_snapshot_is_refused() {
        let mut no_id = projection(vec![segment("seg_1", "trusted_user")]);
        no_id.projection_id = "  ".to_string();
        assert!(matches!(
            validate_projection(no_id, None),
            Err(ContextBridgeError::MalformedProjection { .. })
        ));
        let mut no_snapshot = projection(vec![segment("seg_1", "trusted_user")]);
        no_snapshot.snapshot_id = String::new();
        assert!(matches!(
            validate_projection(no_snapshot, None),
            Err(ContextBridgeError::MalformedProjection { .. })
        ));
    }

    #[test]
    fn a_stale_snapshot_is_refused() {
        let error = validate_projection(
            projection(vec![segment("seg_1", "trusted_user")]),
            Some("snap_2"),
        )
        .expect_err("a projection from an older snapshot must be refused");
        assert_eq!(error.code(), "CONFLICT_STATE");
        assert!(matches!(
            error,
            ContextBridgeError::StaleSnapshot { ref built, ref current }
                if built == "snap_1" && current == "snap_2"
        ));
    }

    #[test]
    fn the_default_port_fails_closed() {
        let port = UnavailableContextBridge;
        let error = futures_lite_block_on(port.project(ProjectionRequest {
            run_id: "run_1".to_string(),
            workspace_id: "ws_1".to_string(),
            program_json: "{}".to_string(),
            snapshot_id: "snap_1".to_string(),
            token_budget: 100,
        }))
        .expect_err("unwired");
        assert_eq!(error.code(), "INTERNAL");
        assert!(matches!(
            error,
            ContextBridgeError::Unavailable { owner, .. } if owner == RPC_BOUNDARY_OWNER
        ));
    }

    fn futures_lite_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}
