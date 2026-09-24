//! The bridge seam and the boundary check (INT-005).

use async_trait::async_trait;
use quansio_contracts::generated::intelligence::{ContextProjection, RenderedMessage};
use quansio_tools::SourceTrust;
use serde_json::{json, Value};

use super::ContextBridgeError;

/// The task that owns the typed RPC boundary the production port crosses.
pub const RPC_BOUNDARY_OWNER: &str = "INT-001";

/// What a run asks the intelligence plane for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectionRequest {
    /// Tenant the run belongs to (the RPC's scope).
    pub tenant_id: String,
    /// Run the projection is for.
    pub run_id: String,
    /// Turn the projection is for.
    pub turn_id: String,
    /// Workspace the run belongs to.
    pub workspace_id: String,
    /// Capability projection the run holds (the RPC's scope).
    pub capability_projection_id: String,
    /// The canonical SearchProgram JSON the plane must execute, produced by the plane's own
    /// authority (`python/intelligence/context/search.py`); the runtime carries it opaquely.
    /// Empty when the projection is built from the supplied segments only.
    pub program_json: String,
    /// Snapshot of the canonical sources the segments were read at.
    pub snapshot_id: String,
    /// Token budget the projection must respect.
    pub token_budget: u32,
    /// Digest of the policy snapshot the run is under.
    pub policy_snapshot_digest: String,
    /// Canonical content the runtime owns and offers for the turn (DOMAIN.md §11.2).
    pub segments: Vec<SegmentRequest>,
    /// The registry's candidate skill versions (`{skill_id, version_id, name, semver, status,
    /// manifest}`), which the plane resolves for the task (INT-009).
    pub skill_candidates: Vec<Value>,
    /// What the task is, for skill relevance.
    pub task_text: String,
    /// The tools the run's capability projection exposes (a skill needing more is excluded).
    pub exposed_tools: Vec<String>,
    /// The effect classes the run's capability projection grants.
    pub granted_effect_classes: Vec<String>,
    /// At most this many skills resolve.
    pub max_skills: u32,
}

/// Where a segment's content came from (DOMAIN.md §11.2 source kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentSource {
    /// The teammate's system instructions.
    System,
    /// Policy the runtime applies.
    Policy,
    /// A thread message.
    Thread,
    /// A WorkGraph item.
    Work,
    /// The result of a tool call.
    ToolResult,
    /// Verified knowledge.
    Knowledge,
    /// Semantic memory.
    Memory,
    /// An artifact's content.
    Artifact,
    /// A search result.
    Search,
}

impl SegmentSource {
    /// The contract spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Policy => "policy",
            Self::Thread => "thread",
            Self::Work => "work",
            Self::ToolResult => "tool_result",
            Self::Knowledge => "knowledge",
            Self::Memory => "memory",
            Self::Artifact => "artifact",
            Self::Search => "search",
        }
    }
}

/// The message role a segment is rendered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentRole {
    /// System instructions.
    System,
    /// The operator channel.
    Operator,
    /// A user turn.
    User,
    /// An assistant turn.
    Assistant,
    /// A tool result.
    Tool,
}

impl SegmentRole {
    /// The contract spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Operator => "operator",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// One piece of canonical content the runtime offers the plane (DOMAIN.md §11.2).
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentRequest {
    /// Order in the turn.
    pub seq: u32,
    /// Where the content came from.
    pub source: SegmentSource,
    /// The canonical record it came from (`msg_…`, `tc_…`, …).
    pub source_ref: String,
    /// The role it is rendered in.
    pub role: SegmentRole,
    /// The content, in the RenderedMessage content shape (a JSON string, or an object with
    /// `parts` and, for tool traffic, `tool_calls` / `tool_call_id` / `tool_name`).
    pub content: Value,
    /// A trust level the plane may only lower the source's label to, never raise it above.
    pub trust_hint: Option<SourceTrust>,
    /// The snapshot the content was read at.
    pub snapshot_ref: String,
    /// Whether the turn cannot proceed without it.
    pub required: bool,
}

impl SegmentRequest {
    /// The `segment_requests_json` entry for this segment.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut request = json!({
            "seq": self.seq,
            "source": {"kind": self.source.as_str(), "ref": self.source_ref},
            "role": self.role.as_str(),
            "content": self.content,
            "snapshot_ref": self.snapshot_ref,
            "required": self.required,
        });
        if let Some(hint) = self.trust_hint {
            request["trust_hint"] = Value::String(hint.as_str().to_string());
        }
        request.to_string()
    }
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
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// What the plane dropped and why (DOMAIN.md §11.2 `degradation`).
    pub degradation: Vec<String>,
    /// The messages the plane rendered, one per segment, in render order.
    pub messages: Vec<RenderedMessage>,
    /// `{budget, used, by_source}` as the plane reported it.
    pub token_ledger: Value,
    /// The policy snapshot the projection was built under.
    pub policy_snapshot_digest: String,
}

impl From<ContextProjection> for ReceivedProjection {
    fn from(projection: ContextProjection) -> Self {
        let tokens_used = projection
            .token_ledger
            .as_ref()
            .map_or(0, |ledger| u32::try_from(ledger.used).unwrap_or(u32::MAX));
        let token_ledger = projection.token_ledger.as_ref().map_or_else(
            || json!({}),
            |ledger| {
                json!({
                    "budget": ledger.budget,
                    "used": ledger.used,
                    "by_source": serde_json::from_str::<Value>(&ledger.by_source_json)
                        .unwrap_or_else(|_| json!({})),
                })
            },
        );
        Self {
            projection_id: projection.id,
            snapshot_id: projection.snapshot_id,
            segments: projection
                .segments
                .into_iter()
                .map(|segment| ProjectionSegment {
                    segment_id: format!("seg_{}", segment.seq),
                    source: format!("{}:{}", segment.source_kind, segment.source_ref),
                    trust_level: segment.trust_level,
                    tokens: u32::try_from(segment.token_estimate).unwrap_or(u32::MAX),
                })
                .collect(),
            tokens_used,
            degraded: !projection.degradation_json.is_empty(),
            degradation: projection.degradation_json,
            messages: projection.messages,
            token_ledger,
            policy_snapshot_digest: projection.policy_snapshot_digest,
        }
    }
}

/// A projection the boundary check accepted, with labels resolved.
#[derive(Debug, Clone, PartialEq)]
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
    /// What the plane dropped and why.
    pub degradation: Vec<String>,
    /// The rendered messages, each at its segment's trust level.
    pub messages: Vec<RenderedMessage>,
    /// `{budget, used, by_source}`.
    pub token_ledger: Value,
    /// The policy snapshot the projection was built under.
    pub policy_snapshot_digest: String,
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
/// must carry a trust level the §12 ladder recognises, the projection must name itself and its
/// snapshot, and every rendered message must be its segment's, at its segment's trust level — so a
/// message cannot reach the model more trusted than its source. `current_snapshot` refuses a
/// projection the sources have moved past.
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
    if received.messages.len() != segments.len() {
        return Err(ContextBridgeError::MalformedProjection {
            detail: format!(
                "{} rendered messages for {} segments",
                received.messages.len(),
                segments.len()
            ),
        });
    }
    for (message, (segment, trust)) in received.messages.iter().zip(&segments) {
        if SourceTrust::parse(&message.trust_level).ok() != Some(*trust) {
            return Err(ContextBridgeError::MalformedProjection {
                detail: format!(
                    "the message for {} is rendered at {:?}, its segment is {}",
                    segment.segment_id,
                    message.trust_level,
                    trust.as_str()
                ),
            });
        }
    }
    Ok(ValidatedProjection {
        projection_id: received.projection_id,
        snapshot_id: received.snapshot_id,
        segments,
        tokens_used: received.tokens_used,
        degraded: received.degraded,
        degradation: received.degradation,
        messages: received.messages,
        token_ledger: received.token_ledger,
        policy_snapshot_digest: received.policy_snapshot_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection(segments: Vec<ProjectionSegment>) -> ReceivedProjection {
        let messages = segments
            .iter()
            .map(|segment| RenderedMessage {
                trust_level: segment.trust_level.clone(),
                ..RenderedMessage::default()
            })
            .collect();
        ReceivedProjection {
            projection_id: "ctx_01J8Z3K6F1N8VQ2X5W9Y0TEST".to_string(),
            snapshot_id: "snap_1".to_string(),
            segments,
            tokens_used: 10,
            messages,
            ..ReceivedProjection::default()
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
    fn every_message_is_its_segments_at_its_segments_trust() {
        let mut missing = projection(vec![segment("seg_1", "trusted_user")]);
        missing.messages.clear();
        assert!(matches!(
            validate_projection(missing, None),
            Err(ContextBridgeError::MalformedProjection { .. })
        ));
        // A tool result rendered as if the user had written it would be promoted trust.
        let mut promoted = projection(vec![segment("seg_1", "untrusted_external")]);
        promoted.messages[0].trust_level = "trusted_user".to_string();
        let error = validate_projection(promoted, None).expect_err("promoted message");
        assert_eq!(error.code(), "VALIDATION_SCHEMA");
    }

    #[test]
    fn a_segment_request_serializes_to_the_contract_shape() {
        let request = SegmentRequest {
            seq: 3,
            source: SegmentSource::ToolResult,
            source_ref: "tc_1".to_string(),
            role: SegmentRole::Tool,
            content: json!({"parts": [], "tool_call_id": "call_1"}),
            trust_hint: Some(SourceTrust::UntrustedExternal),
            snapshot_ref: "run_1@4".to_string(),
            required: false,
        };
        let value: Value = serde_json::from_str(&request.to_json()).expect("json");
        assert_eq!(
            value,
            json!({
                "seq": 3,
                "source": {"kind": "tool_result", "ref": "tc_1"},
                "role": "tool",
                "content": {"parts": [], "tool_call_id": "call_1"},
                "trust_hint": "untrusted_external",
                "snapshot_ref": "run_1@4",
                "required": false,
            })
        );
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
            ..ProjectionRequest::default()
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
