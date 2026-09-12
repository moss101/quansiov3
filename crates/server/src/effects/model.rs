//! Effect Ledger value types (DOMAIN.md §7.1–§7.2).
//!
//! These types mirror the canonical `EffectRecord` shape and the DOMAIN §7.2 state
//! machine exactly; the ledger can only move a record along the transitions listed
//! there. A retry never mutates a record: it creates a new one with a new identity and
//! the same idempotency key.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use quansio_capability::{EffectClass, Tier};
use quansio_core::{Digest, Generation, IdempotencyKey};
use serde::{Deserialize, Serialize};

use crate::effects::error::EffectError;

/// The canonical `EffectRecord.status` values (DOMAIN.md §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EffectStatus {
    /// Proposed but not yet authorized.
    Proposed,
    /// Authorized by policy or an approval receipt.
    Authorized,
    /// Reserved; the action may still be dispatched and has no external effect yet.
    Reserved,
    /// Handed to the target; the external outcome is not settled.
    Dispatched,
    /// The target reported success.
    SettledSuccess,
    /// The target reported failure.
    SettledFailed,
    /// Timeout or disconnect left the external outcome unknown.
    OutcomeUnknown,
    /// Reconciliation is in progress.
    Reconciling,
    /// Reconciliation proved the action landed.
    ReconciledSuccess,
    /// Reconciliation proved the action did not land.
    ReconciledFailed,
    /// No deterministic check exists; a human owns the outcome.
    ReconciliationManual,
    /// Policy denied the action.
    Denied,
    /// The action expired before it was dispatched.
    Expired,
    /// The action was cancelled before dispatch.
    Cancelled,
}

impl EffectStatus {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "PROPOSED",
            Self::Authorized => "AUTHORIZED",
            Self::Reserved => "RESERVED",
            Self::Dispatched => "DISPATCHED",
            Self::SettledSuccess => "SETTLED_SUCCESS",
            Self::SettledFailed => "SETTLED_FAILED",
            Self::OutcomeUnknown => "OUTCOME_UNKNOWN",
            Self::Reconciling => "RECONCILING",
            Self::ReconciledSuccess => "RECONCILED_SUCCESS",
            Self::ReconciledFailed => "RECONCILED_FAILED",
            Self::ReconciliationManual => "RECONCILIATION_MANUAL",
            Self::Denied => "DENIED",
            Self::Expired => "EXPIRED",
            Self::Cancelled => "CANCELLED",
        }
    }

    /// The `effect.<event>` transition that reaches this status (DOMAIN.md §7.2).
    #[must_use]
    pub const fn event_name(self) -> &'static str {
        match self {
            Self::Proposed => "effect.proposed",
            Self::Authorized => "effect.authorized",
            Self::Reserved => "effect.reserved",
            Self::Dispatched => "effect.dispatched",
            Self::SettledSuccess => "effect.settled_success",
            Self::SettledFailed => "effect.settled_failed",
            Self::OutcomeUnknown => "effect.outcome_unknown",
            Self::Reconciling => "effect.reconciling",
            Self::ReconciledSuccess => "effect.reconciled_success",
            Self::ReconciledFailed => "effect.reconciled_failed",
            Self::ReconciliationManual => "effect.reconciliation_manual",
            Self::Denied => "effect.denied",
            Self::Expired => "effect.expired",
            Self::Cancelled => "effect.cancelled",
        }
    }

    /// Parse a persisted status.
    ///
    /// # Errors
    /// Returns [`EffectError::UnknownStatus`] for a value outside DOMAIN.md §7.2.
    pub fn parse(value: &str) -> Result<Self, EffectError> {
        match value {
            "PROPOSED" => Ok(Self::Proposed),
            "AUTHORIZED" => Ok(Self::Authorized),
            "RESERVED" => Ok(Self::Reserved),
            "DISPATCHED" => Ok(Self::Dispatched),
            "SETTLED_SUCCESS" => Ok(Self::SettledSuccess),
            "SETTLED_FAILED" => Ok(Self::SettledFailed),
            "OUTCOME_UNKNOWN" => Ok(Self::OutcomeUnknown),
            "RECONCILING" => Ok(Self::Reconciling),
            "RECONCILED_SUCCESS" => Ok(Self::ReconciledSuccess),
            "RECONCILED_FAILED" => Ok(Self::ReconciledFailed),
            "RECONCILIATION_MANUAL" => Ok(Self::ReconciliationManual),
            "DENIED" => Ok(Self::Denied),
            "EXPIRED" => Ok(Self::Expired),
            "CANCELLED" => Ok(Self::Cancelled),
            other => Err(EffectError::UnknownStatus(other.to_string())),
        }
    }

    /// Whether the record holds a not-yet-dispatched or unresolved external outcome.
    ///
    /// This is exactly the predicate of the partial unique in-flight index
    /// (`effect_records_inflight_key_idx`); a second reservation for the same
    /// `(tenant, effect_class, idempotency_key)` is refused while one is true.
    #[must_use]
    pub const fn is_in_flight(self) -> bool {
        matches!(
            self,
            Self::Reserved | Self::Dispatched | Self::OutcomeUnknown | Self::Reconciling
        )
    }

    /// Whether the external outcome is unknown and must be reconciled, never retried
    /// (DOMAIN.md §7.2; mirrors `protocol_state::is_unsettled`).
    #[must_use]
    pub const fn is_unsettled(self) -> bool {
        matches!(
            self,
            Self::Dispatched | Self::OutcomeUnknown | Self::Reconciling
        )
    }

    /// Whether the record can no longer transition on its own.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::SettledSuccess
                | Self::SettledFailed
                | Self::ReconciledSuccess
                | Self::ReconciledFailed
                | Self::ReconciliationManual
                | Self::Denied
                | Self::Expired
                | Self::Cancelled
        )
    }
}

impl fmt::Display for EffectStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where an effect is dispatched (DOMAIN.md §7.2 `target.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// The server itself (artifact/metadata mutations).
    Server,
    /// A worker daemon inside an execution target.
    Qworkerd,
    /// A managed browser session.
    Browser,
    /// A connector/integration adapter.
    Adapter,
}

impl TargetKind {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Qworkerd => "qworkerd",
            Self::Browser => "browser",
            Self::Adapter => "adapter",
        }
    }

    /// Parse a persisted target kind.
    ///
    /// # Errors
    /// Returns [`EffectError::UnknownTargetKind`] for an unknown value.
    pub fn parse(value: &str) -> Result<Self, EffectError> {
        match value {
            "server" => Ok(Self::Server),
            "qworkerd" => Ok(Self::Qworkerd),
            "browser" => Ok(Self::Browser),
            "adapter" => Ok(Self::Adapter),
            other => Err(EffectError::UnknownTargetKind(other.to_string())),
        }
    }
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The materialized resource an effect acts on (DOMAIN.md §7.2 `resource`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectResource {
    /// Resource kind (`domain`, `path`, `record`, `connector`, …).
    pub kind: String,
    /// Stable selector within the kind.
    pub selector: String,
}

impl EffectResource {
    /// Build a resource.
    #[must_use]
    pub fn new(kind: impl Into<String>, selector: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            selector: selector.into(),
        }
    }

    /// The stable string the idempotency key binds, so two encodings of the same
    /// resource can never produce two keys.
    #[must_use]
    pub fn canonical(&self) -> String {
        format!("{}:{}", self.kind, self.selector)
    }
}

/// The dispatch target of an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectTarget {
    /// Target class.
    pub kind: TargetKind,
    /// Target identity (target id, browser session id, connector id).
    pub id: String,
}

impl EffectTarget {
    /// Build a target.
    #[must_use]
    pub fn new(kind: TargetKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

/// The pending tool call this effect settles, as known by the runtime at reserve time
/// (DOMAIN.md §5.7, §7.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRef {
    /// Tool call id (`tc_…`).
    pub tool_call_id: String,
    /// Tool name, recorded in the run's protocol state.
    pub tool_name: String,
}

impl ToolCallRef {
    /// Build a tool-call reference.
    #[must_use]
    pub fn new(tool_call_id: impl Into<String>, tool_name: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
        }
    }
}

/// How a reservation is authorized (DOSSIER.md §10 steps 3–4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectAuthorization {
    /// Policy allowed the action and the decision is recorded.
    Policy {
        /// `pdc_…` policy decision id.
        decision_id: String,
    },
    /// Policy requires an approval: propose, consume the receipt through the RUN-006
    /// verifier, then reserve. The ledger refuses to reserve without that receipt.
    ApprovalRequired,
}

/// How the external effect ended (DOMAIN.md §7.2 `outcome.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    /// The target reported success.
    Success,
    /// The target reported a failure that may be retried after settlement.
    FailedRetryable,
    /// The target reported a permanent failure.
    FailedPermanent,
}

impl OutcomeKind {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::FailedRetryable => "failed_retryable",
            Self::FailedPermanent => "failed_permanent",
        }
    }

    /// Whether a settled failure with this classification may be retried.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::FailedRetryable)
    }
}

impl fmt::Display for OutcomeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The recorded outcome of a settled or reconciled effect (DOMAIN.md §7.2 `outcome`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectOutcome {
    /// Outcome classification.
    pub kind: OutcomeKind,
    /// Remote reference returned by the external system, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_ref: Option<String>,
    /// Evidence ids proving the outcome.
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

impl EffectOutcome {
    /// A successful outcome.
    #[must_use]
    pub fn success(remote_ref: Option<String>, evidence_ids: Vec<String>) -> Self {
        Self {
            kind: OutcomeKind::Success,
            remote_ref,
            evidence_ids,
        }
    }

    /// A failed outcome with an explicit retry classification.
    #[must_use]
    pub fn failure(retryable: bool, remote_ref: Option<String>, evidence_ids: Vec<String>) -> Self {
        Self {
            kind: if retryable {
                OutcomeKind::FailedRetryable
            } else {
                OutcomeKind::FailedPermanent
            },
            remote_ref,
            evidence_ids,
        }
    }

    /// Whether a retry may be attempted for this outcome.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}

/// How an unknown effect may be reconciled (DOMAIN.md §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationStrategy {
    /// No reconciliation is possible; the outcome must be handled manually.
    None,
    /// Re-applying the same idempotency key is a safe deterministic check.
    Idempotent,
    /// The external system can be queried by the class's key.
    Query,
    /// Only a human can resolve the outcome.
    Manual,
}

impl ReconciliationStrategy {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Idempotent => "idempotent",
            Self::Query => "query",
            Self::Manual => "manual",
        }
    }

    /// Parse a DOMAIN.md §7.1 strategy cell, keeping the reason text out of the enum.
    ///
    /// # Errors
    /// Returns [`EffectError::UnknownReconciliationStrategy`] for an unknown strategy.
    pub fn parse(value: &str) -> Result<Self, EffectError> {
        let trimmed = value.trim();
        if trimmed.starts_with("none") {
            Ok(Self::None)
        } else if trimmed.starts_with("idempotent") {
            Ok(Self::Idempotent)
        } else if trimmed.starts_with("query") {
            Ok(Self::Query)
        } else if trimmed.starts_with("manual") {
            Ok(Self::Manual)
        } else {
            Err(EffectError::UnknownReconciliationStrategy(
                trimmed.to_string(),
            ))
        }
    }

    /// Whether the strategy supports a deterministic external check.
    #[must_use]
    pub const fn is_deterministic(self) -> bool {
        matches!(self, Self::Idempotent | Self::Query)
    }
}

impl fmt::Display for ReconciliationStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReconciliationStrategy {
    type Err = EffectError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// The recorded reconciliation state of an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationRecord {
    /// The strategy the class declared.
    pub strategy: ReconciliationStrategy,
    /// How many reconciliation attempts ran.
    pub attempts: u32,
    /// RFC 3339 timestamp of the last attempt.
    pub last_at: DateTime<Utc>,
    /// Result of the last attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// What the caller learned when reconciling an unknown effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciliationEvidence {
    /// A deterministic check (idempotent replay or a query by the class's key) settled
    /// the outcome.
    Determined {
        /// Whether the external action actually landed.
        landed: bool,
        /// Remote reference, when the check returned one.
        remote_ref: Option<String>,
        /// Evidence ids proving the check.
        evidence_ids: Vec<String>,
    },
    /// No deterministic check exists for the class; the record is parked for a human.
    Manual {
        /// Evidence ids for the manual hand-off.
        evidence_ids: Vec<String>,
    },
}

impl ReconciliationEvidence {
    /// A human-readable label used in a strategy-mismatch refusal.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Determined { .. } => "determined",
            Self::Manual { .. } => "manual",
        }
    }
}

/// How a retry is authorized (task RUN-007 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryAuthorization {
    /// A retryable `SETTLED_FAILED` record with a recorded policy decision.
    RecordedDecision,
    /// A fresh policy decision authorizes the new attempt.
    Reauthorized {
        /// `pdc_…` decision id from the fresh evaluation.
        policy_decision_id: String,
    },
}

/// The canonical Effect Ledger row (DOMAIN.md §7.2).
#[derive(Debug, Clone, PartialEq)]
pub struct EffectRecord {
    /// `eff_…` effect id.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Owning run, when the effect belongs to one.
    pub run_id: Option<String>,
    /// Owning step, when the effect belongs to one.
    pub step_id: Option<String>,
    /// Tool call that reserved the effect, when there is one.
    pub tool_call_id: Option<String>,
    /// Semantic effect class.
    pub effect_class: EffectClass,
    /// Consequence tier.
    pub tier: Tier,
    /// Materialized resource.
    pub resource: EffectResource,
    /// Digest of the exact action parameters.
    pub params_digest: Digest,
    /// `(tenant, effect_class, key)` idempotency binding.
    pub idempotency_key: String,
    /// Capability projection the action ran under.
    pub capability_projection_id: String,
    /// Policy decision that allowed the reservation.
    pub policy_decision_id: Option<String>,
    /// Consumed approval receipt, when policy required one.
    pub approval_receipt_id: Option<String>,
    /// Current state.
    pub status: EffectStatus,
    /// Token the host must present to prove it dispatches this reservation.
    pub dispatch_token: Option<String>,
    /// Dispatch target.
    pub target: Option<EffectTarget>,
    /// Recorded outcome, once settled or reconciled.
    pub outcome: Option<EffectOutcome>,
    /// Recorded reconciliation state.
    pub reconciliation: Option<ReconciliationRecord>,
    /// Run/target generation the reservation was issued under.
    pub generation: Generation,
    /// When the reservation happened.
    pub reserved_at: Option<DateTime<Utc>>,
    /// When the effect was dispatched.
    pub dispatched_at: Option<DateTime<Utc>>,
    /// When the effect settled.
    pub settled_at: Option<DateTime<Utc>>,
    /// When the row was created.
    pub created_at: DateTime<Utc>,
}

impl EffectRecord {
    /// The derived idempotency key object for this record.
    #[must_use]
    pub fn idempotency(&self) -> IdempotencyKey {
        IdempotencyKey::derive(
            self.effect_class.as_str(),
            &self.resource.canonical(),
            self.params_digest.clone(),
        )
    }
}

/// A fully specified reservation request.
#[derive(Debug, Clone)]
pub struct NewEffect {
    /// Owning workspace.
    pub workspace_id: String,
    /// Owning run, when the effect belongs to one.
    pub run_id: Option<String>,
    /// Owning step, when the effect belongs to one.
    pub step_id: Option<String>,
    /// Pending tool call this effect settles, when there is one.
    pub tool: Option<ToolCallRef>,
    /// Semantic effect class.
    pub effect_class: EffectClass,
    /// Consequence tier; must equal the registered tier for the class.
    pub tier: Tier,
    /// Materialized resource.
    pub resource: EffectResource,
    /// Digest of the exact action parameters.
    pub params_digest: Digest,
    /// Capability projection the action runs under.
    pub capability_projection_id: String,
    /// Authorization for the reservation.
    pub authorization: EffectAuthorization,
    /// Dispatch target.
    pub target: EffectTarget,
    /// Run/target generation.
    pub generation: Generation,
}

impl NewEffect {
    /// Build a reservation request for the common policy-authorized path.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn policy_authorized(
        workspace_id: impl Into<String>,
        effect_class: EffectClass,
        tier: Tier,
        resource: EffectResource,
        params_digest: Digest,
        capability_projection_id: impl Into<String>,
        decision_id: impl Into<String>,
        target: EffectTarget,
        generation: Generation,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            run_id: None,
            step_id: None,
            tool: None,
            effect_class,
            tier,
            resource,
            params_digest,
            capability_projection_id: capability_projection_id.into(),
            authorization: EffectAuthorization::Policy {
                decision_id: decision_id.into(),
            },
            target,
            generation,
        }
    }

    /// Attach the owning run and step.
    #[must_use]
    pub fn with_run(mut self, run_id: impl Into<String>, step_id: Option<String>) -> Self {
        self.run_id = Some(run_id.into());
        self.step_id = step_id;
        self
    }

    /// Attach the pending tool call this effect settles.
    #[must_use]
    pub fn with_tool(mut self, tool: ToolCallRef) -> Self {
        self.tool = Some(tool);
        self
    }
}
