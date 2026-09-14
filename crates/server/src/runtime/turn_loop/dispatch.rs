//! The ToolCall protocol (DOMAIN.md §7.4): validate, authorize, reserve and dispatch.
//!
//! One call moves through a fixed, fail-closed order:
//!
//! ```text
//! registry resolution and strict schema validation    (crates/tools)
//! → effect class, resource and idempotency derivation  (crates/tools)
//! → CapabilityProjection authorize                     (RUN-005)
//! → policy, RBAC, privacy guards, user rules           (RUN-006)
//! → ApprovalRequest and WAITING_APPROVAL when required
//! → EffectRecord RESERVED with a dispatch token         (RUN-007)
//! → dispatch to the declared host, settle the effect
//! ```
//!
//! A refusal before the reservation writes no EffectRecord and never reaches a host. An
//! unknown outcome after a dispatch is recorded as `OUTCOME_UNKNOWN` and is only ever
//! resolved by reconciliation, so a resumed runtime cannot duplicate an external effect.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use quansio_capability::{
    authorize, Approval as GrantApproval, AuthorizationRequest, CapabilityProjection, Decision,
    EffectClass, Grant, ProjectionInput, ProjectionSubject, SubjectKind, Tier,
};
use quansio_core::{CanonicalId, Digest, Generation, Prefix, UlidGenerator};
use quansio_events::event_type::EventType;
use quansio_events::{EventDraft, EventStore};
use quansio_tools::{
    canonical_json, plan_call, CallContext, SourceTrust, ToolCallPlan, ToolRegistry,
};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use crate::control::schema;
use crate::effects::{
    EffectAuthorization, EffectError, EffectLedger, EffectResource, EffectStatus, EffectTarget,
    NewEffect, TargetKind, ToolCallRef,
};
use crate::policy::{
    ActionFamily, ActorRoles, ApprovalSigner, ConsequencePreview, DataClass, DispatchBinding,
    EgressGrantSet, EvaluationRequest, NewApprovalRequest, PolicyEvaluator, PolicyOutcome,
    PolicyStore, SequenceContext, TrustLevel, UntrustedOrigin, FAIL_CLOSED_TRUST,
};
use crate::runtime::planning::PLAN_GRAPH_OWNER;
use crate::runtime::protocol_state::{is_unsettled, PendingToolCall, ProtocolState};

use super::super::state_machine::{
    DelegationContext, DelegationOutcome, DelegationPort, DelegationRequest, ProposedToolCall, Run,
    RunStatus, RuntimeError, RuntimeIdentity, RuntimeStore, ToolDispatchOutcome, ToolDispatchPort,
    ToolDispatchRequest, MEMORY_OWNER,
};
use super::questions::{NewQuestion, QuestionService};

/// The task that owns the file, terminal and process tool host.
pub const FILE_TOOL_HOST_OWNER: &str = "EXEC-006";
/// The task that owns managed browser sessions.
pub const BROWSER_TOOL_HOST_OWNER: &str = "EXEC-009";
/// The task that owns native computer-use through the Rust machine authority.
pub const COMPUTER_TOOL_HOST_OWNER: &str = "EXEC-010";
/// The task that owns the connector and integration broker.
pub const CONNECTOR_HOST_OWNER: &str = "EXEC-011";
/// The task that owns source-control, PR and CI tools.
pub const SCM_HOST_OWNER: &str = "EXEC-012";
/// The task that owns workspace artifacts (server-hosted artifact tools).
pub const ARTIFACT_HOST_OWNER: &str = "CORE-007";
/// The task that owns ACTIVE knowledge (the server-hosted citation tool).
pub const KNOWLEDGE_HOST_OWNER: &str = "INT-006";
/// The task that resolves the acting user's roles and identity.
pub const ROLE_PROVIDER_OWNER: &str = "APP-002";

/// The task that owns an execution host for a tool.
#[must_use]
pub fn host_owner(tool: &str) -> &'static str {
    match tool {
        "artifact.create" | "artifact.update" => ARTIFACT_HOST_OWNER,
        "knowledge.cite" => KNOWLEDGE_HOST_OWNER,
        "browser.navigate" | "browser.click" | "browser.type" | "browser.extract"
        | "browser.screenshot" => BROWSER_TOOL_HOST_OWNER,
        _ if tool.starts_with("computer.") => COMPUTER_TOOL_HOST_OWNER,
        "web.search" | "web.fetch" => CONNECTOR_HOST_OWNER,
        _ if tool.starts_with("connector.") => CONNECTOR_HOST_OWNER,
        _ if tool.starts_with("scm.") => SCM_HOST_OWNER,
        _ => FILE_TOOL_HOST_OWNER,
    }
}

/// What a host returned for a dispatched call.
#[derive(Debug, Clone, PartialEq)]
pub enum HostOutcome {
    /// The host completed the call.
    Success {
        /// Output the host produced; the runtime bounds it to `max_output_bytes`.
        output: Value,
        /// Evidence ids the host captured.
        evidence_ids: Vec<String>,
        /// Remote identity of the effect, for reconciliation.
        remote_ref: Option<String>,
    },
    /// The host ran and failed with a known outcome.
    Failure {
        /// Whether a retry could succeed.
        retryable: bool,
        /// Typed error.
        error: Value,
        /// Evidence ids the host captured.
        evidence_ids: Vec<String>,
    },
    /// The host could not establish the outcome; the effect must be reconciled.
    Unknown {
        /// Why the outcome is unknown.
        detail: String,
    },
}

/// A host refusing to take the call at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFailure {
    /// The host is not wired yet; the named task owns it.
    Unavailable {
        /// Owning task.
        owner: &'static str,
        /// What is missing.
        detail: String,
    },
    /// The host refused the call (for example, a sandbox policy).
    Refused {
        /// Why.
        detail: String,
    },
}

impl HostFailure {
    /// The typed Quansio error code (DOMAIN.md §15).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "INTERNAL",
            Self::Refused { .. } => "POLICY_DENIED",
        }
    }
}

/// A call handed to a tool host for execution.
#[derive(Debug, Clone, PartialEq)]
pub struct HostDispatch {
    /// `tc_…` tool call row identity.
    pub tool_call_id: String,
    /// Registered tool name.
    pub tool: String,
    /// Declaration version the call was planned against.
    pub version: u32,
    /// Validated arguments.
    pub args: Value,
    /// Effect the dispatch settles.
    pub effect_id: String,
    /// Fence token the host must echo back.
    pub dispatch_token: String,
    /// Run the call belongs to.
    pub run_id: String,
    /// Step the call is recorded under.
    pub step_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Upper bound on output returned to the model.
    pub max_output_bytes: usize,
    /// Dispatch deadline.
    pub timeout_ms: u64,
}

/// Executes a tool on its declared host.
#[async_trait]
pub trait ToolHostPort: Send + Sync {
    /// Run the dispatch on its host.
    ///
    /// # Errors
    /// Returns [`HostFailure`] when the host refuses or is not available.
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure>;
}

/// The default host: every execution host belongs to a later EXEC task, so a call to one
/// fails closed with the owning task named instead of simulating a result.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableToolHost;

#[async_trait]
impl ToolHostPort for UnavailableToolHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        Err(HostFailure::Unavailable {
            owner: host_owner(&dispatch.tool),
            detail: format!("the tool host for {} is not wired yet", dispatch.tool),
        })
    }
}

/// Resolves the capability projection that governs a run's tool calls.
#[async_trait]
pub trait ProjectionProvider: Send + Sync {
    /// The latest projection for a run, or `None` when none was computed.
    async fn projection_for_run(
        &self,
        run_id: &str,
    ) -> Result<Option<CapabilityProjection>, RuntimeError>;
}

/// Reads the durable `capability_projections` row for a run (DOMAIN.md §6.2).
#[derive(Debug, Clone)]
pub struct StoredProjectionProvider {
    pool: PgPool,
    identity: RuntimeIdentity,
}

impl StoredProjectionProvider {
    /// Build the provider for one tenant.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, RuntimeError> {
        schema::validate_tenant_id(&identity.tenant_id)?;
        Ok(Self { pool, identity })
    }
}

#[async_trait]
impl ProjectionProvider for StoredProjectionProvider {
    async fn projection_for_run(
        &self,
        run_id: &str,
    ) -> Result<Option<CapabilityProjection>, RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        let row = sqlx::query(
            "SELECT id, subject_kind, subject_id, inputs, grants, computed_at, expires_at \
             FROM capability_projections \
             WHERE tenant_id = $1 AND subject_kind = 'run' AND subject_id = $2 \
             ORDER BY computed_at DESC, id DESC LIMIT 1",
        )
        .bind(&self.identity.tenant_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(decode_projection).transpose()
    }
}

/// The acting user a run's tool calls are authorized for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActingActor {
    /// Roles the actor holds.
    pub roles: ActorRoles,
    /// The authenticated user, when the run is user-triggered.
    pub user_id: Option<String>,
}

/// Labels the trust of the causal chain a proposal was built from (DOMAIN.md §12).
///
/// The runtime cannot yet prove the chain's trust: INT-005 builds the ContextProjection
/// whose segments carry labels, and INT-012 owns labelling them. Until it does, the
/// default fails closed to [`FAIL_CLOSED_TRUST`], so a tier >= 2 call is escalated and
/// needs an approval rather than silently acting on content nobody vouched for.
pub trait ProposalTrustSource: Send + Sync {
    /// The most-untrusted label of the proposal's causal chain, or `None` when it cannot
    /// be established.
    fn proposal_trust(&self, run_id: &str) -> Option<TrustLevel>;
}

/// The default source: it can prove nothing, so policy evaluates from untrusted origin.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableProposalTrust;

impl ProposalTrustSource for UnavailableProposalTrust {
    fn proposal_trust(&self, _run_id: &str) -> Option<TrustLevel> {
        None
    }
}

/// Resolves the acting user and roles for a run.
#[async_trait]
pub trait RoleProvider: Send + Sync {
    /// The acting actor, or `None` when it cannot be resolved (which denies).
    async fn actor_for_run(&self, run_id: &str) -> Result<Option<ActingActor>, RuntimeError>;
}

/// Default role provider: session identity belongs to [`ROLE_PROVIDER_OWNER`], so an
/// unresolved actor denies instead of assuming a role.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableRoles;

#[async_trait]
impl RoleProvider for UnavailableRoles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(None)
    }
}

/// Applies a validated plan through the canonical graph transaction.
#[async_trait]
pub trait PlanApplicationPort: Send + Sync {
    /// Compile and commit a plan proposal offered by a `work.propose_plan` call.
    ///
    /// # Errors
    /// Returns the typed runtime refusal when the plan cannot be applied.
    async fn apply(
        &self,
        proposal_id: &str,
        proposal_json: &str,
        context: PlanApplicationContext,
    ) -> Result<Value, RuntimeError>;
}

/// Identity a plan application records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanApplicationContext {
    /// Run that proposed the plan.
    pub run_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Agent thread that proposed it.
    pub agent_thread_id: String,
    /// Run generation.
    pub generation: u64,
}

/// Default plan seam: the graph-backed port belongs to [`PLAN_GRAPH_OWNER`].
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailablePlanApplication;

#[async_trait]
impl PlanApplicationPort for UnavailablePlanApplication {
    async fn apply(
        &self,
        _proposal_id: &str,
        _proposal_json: &str,
        _context: PlanApplicationContext,
    ) -> Result<Value, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "plan application",
            PLAN_GRAPH_OWNER,
        ))
    }
}

/// Routes a memory candidate to the gated memory write path.
#[async_trait]
pub trait MemoryProposalPort: Send + Sync {
    /// Offer a candidate; the memory owner decides whether anything is written.
    ///
    /// # Errors
    /// Returns a typed refusal when the memory path cannot accept the candidate.
    async fn propose(&self, context: MemoryProposalContext) -> Result<Value, RuntimeError>;
}

/// Identity a memory proposal records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryProposalContext {
    /// Run that proposed the candidate.
    pub run_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Candidate payload.
    pub candidate_json: String,
    /// Optional scope hint.
    pub scope: Option<String>,
}

/// Default memory seam: gated writes are INT-007's.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableMemoryProposals;

#[async_trait]
impl MemoryProposalPort for UnavailableMemoryProposals {
    async fn propose(&self, _context: MemoryProposalContext) -> Result<Value, RuntimeError> {
        Err(RuntimeError::seam_not_available(
            "memory proposal",
            MEMORY_OWNER,
        ))
    }
}

/// The ToolCall protocol authority.
#[derive(Clone)]
pub struct ToolDispatchService {
    pool: PgPool,
    identity: RuntimeIdentity,
    registry: &'static ToolRegistry,
    runs: RuntimeStore,
    events: EventStore,
    ledger: EffectLedger,
    policies: PolicyStore,
    projections: Arc<dyn ProjectionProvider>,
    roles: Arc<dyn RoleProvider>,
    host: Arc<dyn ToolHostPort>,
    questions: Arc<QuestionService>,
    delegation: Arc<dyn DelegationPort>,
    plans: Arc<dyn PlanApplicationPort>,
    memories: Arc<dyn MemoryProposalPort>,
    trust: Arc<dyn ProposalTrustSource>,
    signer: Option<ApprovalSigner>,
}

impl ToolDispatchService {
    /// Build the service with the capability, role and host seams it must be given.
    ///
    /// # Errors
    /// Returns [`RuntimeError::Schema`] when the tenant id is not canonical.
    pub fn new(
        pool: PgPool,
        identity: RuntimeIdentity,
        projections: Arc<dyn ProjectionProvider>,
        roles: Arc<dyn RoleProvider>,
        host: Arc<dyn ToolHostPort>,
        delegation: Arc<dyn DelegationPort>,
    ) -> Result<Self, RuntimeError> {
        let questions = QuestionService::new(pool.clone(), identity.clone())?;
        let ledger = EffectLedger::new(pool.clone(), identity.clone())
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let policies = PolicyStore::new(pool.clone(), identity.clone())
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        Ok(Self {
            events: EventStore::new(pool.clone()),
            runs: RuntimeStore::new(pool.clone(), identity.clone())?,
            pool,
            identity,
            registry: ToolRegistry::builtin(),
            ledger,
            policies,
            projections,
            roles,
            host,
            questions: Arc::new(questions),
            delegation,
            plans: Arc::new(UnavailablePlanApplication),
            memories: Arc::new(UnavailableMemoryProposals),
            trust: Arc::new(UnavailableProposalTrust),
            signer: ApprovalSigner::from_env(),
        })
    }

    /// Install the plan-application seam (the graph-backed port).
    #[must_use]
    pub fn with_plan_port(mut self, port: Arc<dyn PlanApplicationPort>) -> Self {
        self.plans = port;
        self
    }

    /// Install the memory-proposal seam.
    #[must_use]
    pub fn with_memory_port(mut self, port: Arc<dyn MemoryProposalPort>) -> Self {
        self.memories = port;
        self
    }

    /// Install the proposal-trust source (INT-005's ContextProjection labels).
    #[must_use]
    pub fn with_proposal_trust(mut self, source: Arc<dyn ProposalTrustSource>) -> Self {
        self.trust = source;
        self
    }

    /// Install the approval signer used to resume a parked call.
    #[must_use]
    pub fn with_signer(mut self, signer: ApprovalSigner) -> Self {
        self.signer = Some(signer);
        self
    }

    /// The registry this service plans against.
    #[must_use]
    pub fn registry(&self) -> &'static ToolRegistry {
        self.registry
    }

    /// The Effect Ledger every consequential call reserves through.
    #[must_use]
    pub fn ledger(&self) -> &EffectLedger {
        &self.ledger
    }

    /// The question protocol `user.ask` creates through.
    #[must_use]
    pub fn questions(&self) -> &QuestionService {
        &self.questions
    }

    /// The tools a run's projection exposes (DOMAIN.md §7.5).
    ///
    /// # Errors
    /// Returns a database error when the projection cannot be read.
    pub async fn exposed_tools(
        &self,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, RuntimeError> {
        let Some(projection) = self.projections.projection_for_run(run_id).await? else {
            return Ok(Vec::new());
        };
        Ok(self
            .registry
            .exposed_names(&projection, now)
            .into_iter()
            .map(str::to_string)
            .collect())
    }

    /// Resume a call that parked for approval (DOMAIN.md §7.4, §5.6).
    ///
    /// The call is looked up by the effect it reserved, so a resumed runtime re-uses the
    /// recorded arguments and the recorded declaration version. An unsettled effect is
    /// never re-dispatched; it is reported for reconciliation instead.
    ///
    /// # Errors
    /// Returns a typed error when the effect cannot be reserved or the run is not visible.
    pub async fn resume(
        &self,
        pending: &PendingToolCall,
        request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        if is_unsettled(&pending.effect_status) {
            return Ok(ToolDispatchOutcome::OutcomeUnknown {
                effect_id: Some(pending.effect_id.clone()),
            });
        }
        let Some(stored) = self.tool_call_by_effect(&pending.effect_id).await? else {
            return Ok(ToolDispatchOutcome::Rejected {
                code: "NOT_FOUND".to_string(),
                detail: format!("the tool call for effect {} is gone", pending.effect_id),
            });
        };
        let plan = match plan_call(
            self.registry,
            &stored.tool_name,
            Some(stored.version),
            &stored.args,
            &|class| self.tier_of(class),
            &stored.context.as_context(),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(ToolDispatchOutcome::Rejected {
                    code: error.code().to_string(),
                    detail: error.to_string(),
                })
            }
        };

        // The durable effect status is authoritative: an unsettled record is reported for
        // reconciliation and never re-dispatched.
        let effect = self
            .ledger
            .load(&pending.effect_id)
            .await
            .map_err(effect_refusal)?;
        match effect.status {
            EffectStatus::Authorized => {}
            EffectStatus::Reserved => {}
            status if status.is_unsettled() => {
                return Ok(ToolDispatchOutcome::OutcomeUnknown {
                    effect_id: Some(pending.effect_id.clone()),
                })
            }
            EffectStatus::Proposed => {
                // The run was released by an approval; consume the single-use receipt so
                // the effect becomes AUTHORIZED (DOMAIN.md §7.3).
                let Some(signer) = self.signer.clone() else {
                    return Err(RuntimeError::InvalidArgument(
                        "approval signing key is not configured".to_string(),
                    ));
                };
                let Some(receipt_id) = self.receipt_for_effect(&pending.effect_id).await? else {
                    return Ok(ToolDispatchOutcome::Rejected {
                        code: "APPROVAL_INVALID".to_string(),
                        detail: "no granted approval receipt exists for this call".to_string(),
                    });
                };
                self.policies
                    .verify_and_consume_receipt(
                        &receipt_id,
                        &DispatchBinding {
                            effect_id: pending.effect_id.clone(),
                            params_digest: effect.params_digest.clone(),
                            generation: effect.generation,
                        },
                        &signer,
                        Utc::now(),
                    )
                    .await
                    .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
            }
            other => {
                return Ok(ToolDispatchOutcome::Rejected {
                    code: "CONFLICT_STATE".to_string(),
                    detail: format!(
                        "effect {} is {} and cannot be resumed",
                        pending.effect_id,
                        other.as_str()
                    ),
                })
            }
        }

        let reserved = self
            .ledger
            .load(&pending.effect_id)
            .await
            .map_err(effect_refusal)?;
        let effect = if reserved.status == EffectStatus::Authorized {
            self.ledger
                .reserve_authorized(&pending.effect_id)
                .await
                .map_err(effect_refusal)?
        } else {
            reserved
        };
        let Some(token) = effect.dispatch_token.clone() else {
            return Ok(ToolDispatchOutcome::Rejected {
                code: "CONFLICT_STATE".to_string(),
                detail: "the reserved effect has no dispatch token".to_string(),
            });
        };
        self.ledger
            .mark_dispatched(&pending.effect_id, &token)
            .await
            .map_err(effect_refusal)?;
        let run_id = CanonicalId::parse_typed(&request.run_id, Prefix::Run)?;
        let run = self.runs.load_run(&run_id).await?;
        self.update_tool_call(&stored.id, "dispatched", None, None)
            .await?;
        self.run_to_result(&run, &plan, &stored, request, &pending.effect_id, &token)
            .await
    }

    /// The granted receipt for an effect, when a human approved it.
    async fn receipt_for_effect(&self, effect_id: &str) -> Result<Option<String>, RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        let receipt: Option<String> = sqlx::query_scalar(
            "SELECT r.id FROM approval_receipts r \
             JOIN approval_requests q ON q.id = r.request_id \
             WHERE r.effect_id = $1 AND r.tenant_id = $2 AND q.status = 'granted' \
             ORDER BY r.granted_at DESC LIMIT 1",
        )
        .bind(effect_id)
        .bind(&self.identity.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(receipt)
    }

    fn tier_of(&self, class: &EffectClass) -> Option<Tier> {
        self.ledger.taxonomy().tier_for(class).ok()
    }
}

#[async_trait]
impl ToolDispatchPort for ToolDispatchService {
    async fn dispatch(
        &self,
        call: ProposedToolCall,
        request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        let run_id = CanonicalId::parse_typed(&request.run_id, Prefix::Run)?;
        let generation = Generation::new(request.generation)
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let run = self.runs.load_run(&run_id).await?;
        if run.generation != generation {
            return Err(RuntimeError::FencedStaleGeneration {
                received: generation.get(),
                current: run.generation.get(),
            });
        }
        let context = self.call_context(&run).await?;

        // 1. Resolve and validate strictly. Nothing is written when this refuses.
        let plan = match plan_call(
            self.registry,
            &call.tool,
            None,
            &call.args,
            &|class| self.tier_of(class),
            &context.as_context(),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                return self
                    .reject(&run, &request, &call, error.code(), &error.to_string())
                    .await
            }
        };

        // 2. Capability: the projection must cover the derived effect.
        let Some(projection) = self.projections.projection_for_run(&request.run_id).await? else {
            return self
                .reject(
                    &run,
                    &request,
                    &call,
                    "CAPABILITY_INPUTS_UNAVAILABLE",
                    "the run has no capability projection, so no tool is authorized",
                )
                .await;
        };
        let authorization = match authorize(&projection, &authorization_request(&plan, &projection))
        {
            Ok(authorization) => authorization,
            Err(error) => {
                return self
                    .reject(&run, &request, &call, error.code(), &error.to_string())
                    .await
            }
        };
        if authorization.decision == Decision::Deny {
            return self
                .reject(
                    &run,
                    &request,
                    &call,
                    "CAPABILITY_DENIED",
                    &format!(
                        "the capability projection refuses {}: {}",
                        plan.effect_class.as_str(),
                        authorization.reason.as_str()
                    ),
                )
                .await;
        }

        // 3. Policy, RBAC, privacy guards and user rules.
        let Some(actor) = self.roles.actor_for_run(&request.run_id).await? else {
            return self
                .reject(
                    &run,
                    &request,
                    &call,
                    "POLICY_DENIED",
                    &format!("the acting actor is unresolved; owned by {ROLE_PROVIDER_OWNER}"),
                )
                .await;
        };
        // Policy evaluates the proposal's origin: an unprovable chain fails closed to
        // UNTRUSTED_EXTERNAL, which escalates a tier >= 2 call one tier and forbids
        // `always` rules (DOMAIN.md §12 rule 2).
        let derived = self
            .trust
            .proposal_trust(&request.run_id)
            .unwrap_or(FAIL_CLOSED_TRUST);
        let outcome = self
            .evaluate_policy(&run, &plan, &actor, derived, &projection.id.to_string())
            .await?;
        if outcome.is_deny() {
            return self
                .reject(
                    &run,
                    &request,
                    &call,
                    "POLICY_DENIED",
                    &format!("policy denied the call: {}", outcome.reason.as_str()),
                )
                .await;
        }

        // 4. Consequential work goes through the Effect Ledger from here on.
        let tool_call_id = self
            .insert_tool_call(&run, &request, &plan, "validated", None)
            .await?;
        let decision = self
            .policies
            .record_decision(
                &run.workspace_id,
                Some(&request.run_id),
                None,
                Some(&projection.id.to_string()),
                &outcome,
            )
            .await
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;

        if outcome.requires_approval() {
            return self
                .park_for_approval(&run, &request, &plan, &projection, &tool_call_id, &outcome)
                .await;
        }

        let effect = match self
            .ledger
            .reserve(
                NewEffect::policy_authorized(
                    run.workspace_id.clone(),
                    plan.effect_class.clone(),
                    plan.tier,
                    effect_resource(&plan),
                    plan.params_digest.clone(),
                    projection.id.to_string(),
                    decision.id,
                    EffectTarget::new(TargetKind::Server, tool_target(&plan)),
                    run.generation,
                )
                .with_run(request.run_id.clone(), Some(request.step_id.clone()))
                .with_tool(ToolCallRef::new(&tool_call_id, &plan.tool)),
            )
            .await
        {
            Ok(effect) => effect,
            Err(EffectError::DuplicateInFlight {
                existing_effect_id, ..
            }) => {
                // One in-flight record per idempotency key: the duplicate is refused, never
                // dispatched twice (DOMAIN.md §7.2).
                self.update_tool_call(
                    &tool_call_id,
                    "rejected",
                    Some(&existing_effect_id),
                    Some(json!({ "code": "CONFLICT_IDEMPOTENCY_MISMATCH" })),
                )
                .await?;
                return Ok(ToolDispatchOutcome::Rejected {
                    code: "CONFLICT_IDEMPOTENCY_MISMATCH".to_string(),
                    detail: format!("effect {existing_effect_id} is already in flight"),
                });
            }
            Err(error) => return Err(effect_refusal(error)),
        };
        let Some(token) = effect.dispatch_token.clone() else {
            return Err(RuntimeError::InvalidArgument(
                "a reserved effect has no dispatch token".to_string(),
            ));
        };
        // The fence token moves the reservation to DISPATCHED; nothing settles from
        // RESERVED, so a crash before this point leaves a record that must be reconciled.
        self.ledger
            .mark_dispatched(&effect.id, &token)
            .await
            .map_err(effect_refusal)?;
        self.update_tool_call(&tool_call_id, "dispatched", Some(&effect.id), None)
            .await?;
        self.emit_tool_event(
            &run,
            &tool_call_id,
            "tool.dispatched",
            Some(plan.effect_class.as_str()),
            json!({
                "tool_call_id": tool_call_id,
                "tool": plan.tool,
                "effect_class": plan.effect_class.as_str(),
                "effect_id": effect.id,
                "dispatch_token": token,
                "host": plan.host.as_str(),
            }),
        )
        .await?;

        let stored = StoredCall {
            id: tool_call_id,
            tool_name: plan.tool.clone(),
            version: plan.version,
            args: plan.args.clone(),
            context,
        };
        self.run_to_result(&run, &plan, &stored, request, &effect.id, &token)
            .await
    }

    async fn resume(
        &self,
        pending: PendingToolCall,
        request: ToolDispatchRequest,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        ToolDispatchService::resume(self, &pending, request).await
    }
}

impl ToolDispatchService {
    /// Dispatch a reserved effect to its host and settle what the host reports.
    async fn run_to_result(
        &self,
        run: &Run,
        plan: &ToolCallPlan,
        stored: &StoredCall,
        request: ToolDispatchRequest,
        effect_id: &str,
        token: &str,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        if is_internal_tool(&plan.tool) {
            return self
                .run_internal(run, plan, stored, &request, effect_id)
                .await;
        }
        let dispatch = HostDispatch {
            tool_call_id: stored.id.clone(),
            tool: stored.tool_name.clone(),
            version: stored.version,
            args: stored.args.clone(),
            effect_id: effect_id.to_string(),
            dispatch_token: token.to_string(),
            run_id: request.run_id.clone(),
            step_id: request.step_id.clone(),
            workspace_id: run.workspace_id.clone(),
            max_output_bytes: plan.max_output_bytes,
            timeout_ms: plan.timeout_ms,
        };
        match self.host.execute(dispatch).await {
            Ok(HostOutcome::Success {
                output,
                evidence_ids,
                remote_ref,
            }) => {
                let bounded = bound_output(output, plan.max_output_bytes);
                self.ledger
                    .settle_success(effect_id, remote_ref, evidence_ids.clone())
                    .await
                    .map_err(effect_refusal)?;
                self.update_tool_call(&stored.id, "completed", None, Some(bounded.clone()))
                    .await?;
                self.emit_tool_event(
                    run,
                    &stored.id,
                    "tool.completed",
                    Some(plan.effect_class.as_str()),
                    json!({
                        "tool_call_id": stored.id,
                        "tool": plan.tool,
                        "effect_id": effect_id,
                        "evidence_ids": evidence_ids,
                        "truncated": bounded.get("truncated").is_some(),
                    }),
                )
                .await?;
                Ok(ToolDispatchOutcome::Completed {
                    evidence_ids,
                    effect_id: Some(effect_id.to_string()),
                })
            }
            Ok(HostOutcome::Failure {
                retryable,
                error,
                evidence_ids,
            }) => {
                self.ledger
                    .settle_failure(effect_id, retryable, None, evidence_ids)
                    .await
                    .map_err(effect_refusal)?;
                self.update_tool_call(&stored.id, "failed", None, Some(error.clone()))
                    .await?;
                self.emit_tool_event(
                    run,
                    &stored.id,
                    "tool.failed",
                    Some(plan.effect_class.as_str()),
                    json!({
                        "tool_call_id": stored.id,
                        "tool": plan.tool,
                        "effect_id": effect_id,
                        "retryable": retryable,
                    }),
                )
                .await?;
                Ok(ToolDispatchOutcome::Failed { error })
            }
            Ok(HostOutcome::Unknown { detail }) => {
                self.mark_unknown(run, &stored.id, effect_id, plan, &detail)
                    .await?;
                Ok(ToolDispatchOutcome::OutcomeUnknown {
                    effect_id: Some(effect_id.to_string()),
                })
            }
            Err(failure) => {
                // Nothing external happened, so no outcome is unknown: the reservation is
                // released and the call reports the refusal.
                self.ledger
                    .cancel(effect_id)
                    .await
                    .map_err(effect_refusal)?;
                self.update_tool_call(
                    &stored.id,
                    "rejected",
                    None,
                    Some(json!({ "code": failure.code(), "detail": failure_detail(&failure) })),
                )
                .await?;
                Ok(ToolDispatchOutcome::Rejected {
                    code: failure.code().to_string(),
                    detail: failure_detail(&failure),
                })
            }
        }
    }

    /// Run a control-plane tool whose handler is a runtime port rather than a host
    /// (DOMAIN.md §7.5 `user.ask`, `work.*`, `memory.propose`).
    ///
    /// The effect is settled here because the record the call creates *is* the effect; the
    /// run's subsequent waiting state is recorded by the engine.
    async fn run_internal(
        &self,
        run: &Run,
        plan: &ToolCallPlan,
        stored: &StoredCall,
        request: &ToolDispatchRequest,
        effect_id: &str,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        match plan.tool.as_str() {
            "user.ask" => {
                let ttl = self.questions.ttl_seconds(&run.workspace_id).await?;
                let question = self
                    .questions
                    .create(NewQuestion {
                        run_id: request.run_id.clone(),
                        workspace_id: run.workspace_id.clone(),
                        step_id: Some(request.step_id.clone()),
                        thread_id: None,
                        kind: string_arg(&stored.args, "kind")
                            .unwrap_or_else(|| "free_text".into()),
                        prompt: string_arg(&stored.args, "prompt").unwrap_or_default(),
                        options: string_array_arg(&stored.args, "choices"),
                        required: stored
                            .args
                            .get("required")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        ttl_seconds: ttl,
                        generation: Some(run.generation),
                    })
                    .await?;
                self.settle_internal(
                    run,
                    plan,
                    stored,
                    effect_id,
                    Some(question.id.clone()),
                    json!({ "question_id": question.id, "status": question.status.as_str() }),
                )
                .await?;
                Ok(ToolDispatchOutcome::Parked {
                    state: RunStatus::WaitingQuestion,
                    wait_key: question.id,
                    effect_id: Some(effect_id.to_string()),
                })
            }
            "work.delegate" => {
                let delegation = DelegationRequest {
                    request_id: stored.id.clone(),
                    work_node_id: string_arg(&stored.args, "work_node_id"),
                    instruction: string_arg(&stored.args, "instruction").unwrap_or_default(),
                };
                match self
                    .delegation
                    .delegate(
                        delegation,
                        DelegationContext {
                            run_id: request.run_id.clone(),
                            generation: run.generation.get(),
                        },
                    )
                    .await
                {
                    Ok(DelegationOutcome::Spawned {
                        child_agent_thread_id,
                        ..
                    }) => {
                        self.settle_internal(
                            run,
                            plan,
                            stored,
                            effect_id,
                            Some(child_agent_thread_id.clone()),
                            json!({ "child_agent_thread_id": child_agent_thread_id }),
                        )
                        .await?;
                        Ok(ToolDispatchOutcome::Parked {
                            state: RunStatus::WaitingChild,
                            wait_key: child_agent_thread_id,
                            effect_id: Some(effect_id.to_string()),
                        })
                    }
                    Ok(DelegationOutcome::Joined { result }) => {
                        self.settle_internal(run, plan, stored, effect_id, None, result.clone())
                            .await?;
                        Ok(ToolDispatchOutcome::Completed {
                            evidence_ids: Vec::new(),
                            effect_id: Some(effect_id.to_string()),
                        })
                    }
                    Err(error) => self.refuse_internal(run, stored, effect_id, error).await,
                }
            }
            "work.propose_plan" => {
                let proposal_id =
                    string_arg(&stored.args, "proposal_id").unwrap_or_else(|| stored.id.clone());
                let proposal_json = string_arg(&stored.args, "proposal_json").unwrap_or_default();
                match self
                    .plans
                    .apply(
                        &proposal_id,
                        &proposal_json,
                        PlanApplicationContext {
                            run_id: request.run_id.clone(),
                            workspace_id: run.workspace_id.clone(),
                            agent_thread_id: run.agent_thread_id.to_string(),
                            generation: run.generation.get(),
                        },
                    )
                    .await
                {
                    Ok(outcome) => {
                        self.settle_internal(run, plan, stored, effect_id, None, outcome.clone())
                            .await?;
                        Ok(ToolDispatchOutcome::Completed {
                            evidence_ids: Vec::new(),
                            effect_id: Some(effect_id.to_string()),
                        })
                    }
                    Err(error) => self.refuse_internal(run, stored, effect_id, error).await,
                }
            }
            "memory.propose" => {
                match self
                    .memories
                    .propose(MemoryProposalContext {
                        run_id: request.run_id.clone(),
                        workspace_id: run.workspace_id.clone(),
                        candidate_json: string_arg(&stored.args, "candidate_json")
                            .unwrap_or_default(),
                        scope: string_arg(&stored.args, "scope"),
                    })
                    .await
                {
                    Ok(outcome) => {
                        self.settle_internal(run, plan, stored, effect_id, None, outcome.clone())
                            .await?;
                        Ok(ToolDispatchOutcome::Completed {
                            evidence_ids: Vec::new(),
                            effect_id: Some(effect_id.to_string()),
                        })
                    }
                    Err(error) => self.refuse_internal(run, stored, effect_id, error).await,
                }
            }
            other => Err(RuntimeError::InvalidArgument(format!(
                "tool {other} is not an internal control tool"
            ))),
        }
    }

    async fn settle_internal(
        &self,
        run: &Run,
        plan: &ToolCallPlan,
        stored: &StoredCall,
        effect_id: &str,
        remote_ref: Option<String>,
        result: Value,
    ) -> Result<(), RuntimeError> {
        self.ledger
            .settle_success(effect_id, remote_ref, Vec::new())
            .await
            .map_err(effect_refusal)?;
        self.update_tool_call(&stored.id, "completed", None, Some(result))
            .await?;
        self.emit_tool_event(
            run,
            &stored.id,
            "tool.completed",
            Some(plan.effect_class.as_str()),
            json!({
                "tool_call_id": stored.id,
                "tool": plan.tool,
                "effect_id": effect_id,
            }),
        )
        .await
    }

    async fn refuse_internal(
        &self,
        run: &Run,
        stored: &StoredCall,
        effect_id: &str,
        error: RuntimeError,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        self.ledger
            .cancel(effect_id)
            .await
            .map_err(effect_refusal)?;
        self.update_tool_call(
            &stored.id,
            "rejected",
            None,
            Some(json!({ "code": error.code(), "detail": error.to_string() })),
        )
        .await?;
        self.emit_tool_event(
            run,
            &stored.id,
            "tool.rejected",
            None,
            json!({
                "tool_call_id": stored.id,
                "tool": stored.tool_name,
                "code": error.code(),
                "detail": error.to_string(),
            }),
        )
        .await?;
        Ok(ToolDispatchOutcome::Rejected {
            code: error.code().to_string(),
            detail: error.to_string(),
        })
    }

    async fn mark_unknown(
        &self,
        run: &Run,
        tool_call_id: &str,
        effect_id: &str,
        plan: &ToolCallPlan,
        detail: &str,
    ) -> Result<(), RuntimeError> {
        self.ledger
            .mark_outcome_unknown(effect_id, Some(detail.to_string()))
            .await
            .map_err(effect_refusal)?;
        self.update_tool_call(
            tool_call_id,
            "unknown",
            None,
            Some(json!({ "detail": detail })),
        )
        .await?;
        self.emit_tool_event(
            run,
            tool_call_id,
            "tool.outcome_unknown",
            Some(plan.effect_class.as_str()),
            json!({
                "tool_call_id": tool_call_id,
                "tool": plan.tool,
                "effect_id": effect_id,
                "detail": detail,
            }),
        )
        .await
    }

    async fn call_context(&self, run: &Run) -> Result<CallContextOwned, RuntimeError> {
        Ok(CallContextOwned {
            workspace_id: run.workspace_id.clone(),
            run_id: Some(run.id.to_string()),
            fs_root: self.execution_root(run).await?,
            own_branch: None,
        })
    }

    /// The execution target's workspace root, when the target reports one.
    async fn execution_root(&self, run: &Run) -> Result<Option<String>, RuntimeError> {
        let Some(target_id) = &run.execution_target_id else {
            return Ok(None);
        };
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        let root: Option<String> = sqlx::query_scalar(
            "SELECT resources ->> 'workspace_root' FROM execution_targets \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(&self.identity.tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        tx.commit().await?;
        Ok(root)
    }

    async fn evaluate_policy(
        &self,
        run: &Run,
        plan: &ToolCallPlan,
        actor: &ActingActor,
        derived_trust: TrustLevel,
        capability_projection_id: &str,
    ) -> Result<PolicyOutcome, RuntimeError> {
        let policies = self
            .policies
            .load_policy_set(&run.workspace_id)
            .await
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let user_rules = match &actor.user_id {
            Some(user_id) => self
                .policies
                .load_user_rules(&run.workspace_id, user_id)
                .await
                .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?,
            // No authenticated user means no user rule can apply; user rules can only ever
            // relax a decision that policy already permits, so omitting them fails closed.
            None => Vec::new(),
        };
        let evaluator = PolicyEvaluator::new(&policies, &user_rules);
        let egress = EgressGrantSet::empty();
        let data_classes: Vec<DataClass> = Vec::new();
        let sequence_guards = Vec::new();
        let sequence = SequenceContext::empty();
        let request = EvaluationRequest {
            effect_class: &plan.effect_class,
            resource: &plan.resource,
            catalog_tier: plan.tier,
            action: ActionFamily::ExecuteEffect {
                tier: plan.tier.get(),
            },
            roles: actor.roles,
            derived_from_trust: Some(derived_trust),
            data_classes: &data_classes,
            destination: None,
            egress_grants: &egress,
            // Policy fails closed without the projection that authorized the call.
            capability_projection_id: Some(capability_projection_id),
            sequence_guards: &sequence_guards,
            sequence: &sequence,
            user_id: actor.user_id.as_deref(),
            now: Utc::now(),
        };
        Ok(evaluator.evaluate(&request))
    }

    /// The effective `approval_default_ttl` for a workspace, defaulting to one hour.
    async fn approval_ttl_seconds(&self, workspace_id: &str) -> Result<i64, RuntimeError> {
        let policies = self
            .policies
            .load_policy_set(workspace_id)
            .await
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        let ttl = policies
            .in_order()
            .iter()
            .map(|policy| policy.approval_default_ttl_seconds)
            .min()
            .filter(|seconds| *seconds > 0)
            .unwrap_or(3600);
        Ok(ttl)
    }

    async fn park_for_approval(
        &self,
        run: &Run,
        request: &ToolDispatchRequest,
        plan: &ToolCallPlan,
        projection: &CapabilityProjection,
        tool_call_id: &str,
        outcome: &PolicyOutcome,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        let effect = self
            .ledger
            .propose(NewEffect {
                workspace_id: run.workspace_id.clone(),
                run_id: Some(request.run_id.clone()),
                step_id: Some(request.step_id.clone()),
                tool: Some(ToolCallRef::new(tool_call_id, &plan.tool)),
                effect_class: plan.effect_class.clone(),
                tier: plan.tier,
                resource: effect_resource(plan),
                params_digest: plan.params_digest.clone(),
                capability_projection_id: projection.id.to_string(),
                // An approval-required call is only PROPOSED here; the single-use receipt
                // is what moves it to AUTHORIZED and lets the reservation happen.
                authorization: EffectAuthorization::ApprovalRequired,
                target: EffectTarget::new(TargetKind::Server, tool_target(plan)),
                generation: run.generation,
            })
            .await
            .map_err(effect_refusal)?;

        let ttl = self.approval_ttl_seconds(&run.workspace_id).await?;
        // The operator sees what they are approving: the tool, and — when policy escalated the
        // proposal because its chain reached untrusted content — the untrusted origin itself, so a
        // page-supplied instruction can never be presented as an ordinary request (INT-012).
        let preview = approval_preview(
            &plan.tool,
            outcome.escalated,
            vec![
                format!("tool_call:{tool_call_id}"),
                format!("run:{}", run.id),
            ],
        );
        let approval = NewApprovalRequest::new(
            request.run_id.clone(),
            run.workspace_id.clone(),
            effect.id.clone(),
            Vec::new(),
            format!("Approve {} ({})", plan.tool, plan.effect_class.as_str()),
            preview,
            plan.params_digest.clone(),
            projection.id.to_string(),
            Utc::now() + chrono::Duration::seconds(ttl),
        );
        let record = self
            .policies
            .create_approval_request(&approval)
            .await
            .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
        self.update_tool_call(tool_call_id, "validated", Some(&effect.id), None)
            .await?;

        // ProtocolState records the pending call with its dispatch token, so a resumed
        // runtime continues this call instead of proposing it again.
        let mut state = self
            .runs
            .load_protocol_state(&run.id)
            .await?
            .unwrap_or_else(|| ProtocolState::new(run.id.to_string(), run.generation.get() as i64));
        state.generation = run.generation.get() as i64;
        state.pending_tool_calls.push(PendingToolCall {
            tool_call_id: tool_call_id.to_string(),
            tool_name: plan.tool.clone(),
            dispatch_token: effect.dispatch_token.clone().unwrap_or_default(),
            effect_id: effect.id.clone(),
            effect_status: "PROPOSED".to_string(),
        });
        self.runs.store_protocol_state(&state).await?;

        Ok(ToolDispatchOutcome::Parked {
            state: RunStatus::WaitingApproval,
            wait_key: record.id.to_string(),
            effect_id: Some(effect.id),
        })
    }

    async fn reject(
        &self,
        run: &Run,
        request: &ToolDispatchRequest,
        call: &ProposedToolCall,
        code: &str,
        detail: &str,
    ) -> Result<ToolDispatchOutcome, RuntimeError> {
        let id = self
            .insert_raw_call(
                request,
                &call.tool,
                1,
                &call.args,
                "rejected",
                Some(json!({ "code": code, "detail": detail })),
                SourceTrust::UntrustedExternal.rank(),
            )
            .await?;
        self.emit_tool_event(
            run,
            &id,
            "tool.rejected",
            None,
            json!({
                "tool_call_id": id,
                "tool": call.tool,
                "code": code,
                "detail": detail,
            }),
        )
        .await?;
        Ok(ToolDispatchOutcome::Rejected {
            code: code.to_string(),
            detail: detail.to_string(),
        })
    }

    async fn insert_tool_call(
        &self,
        run: &Run,
        request: &ToolDispatchRequest,
        plan: &ToolCallPlan,
        status: &str,
        result: Option<Value>,
    ) -> Result<String, RuntimeError> {
        let _ = run;
        self.insert_raw_call(
            request,
            &plan.tool,
            plan.version,
            &plan.args,
            status,
            result,
            plan.source_trust.rank(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_raw_call(
        &self,
        request: &ToolDispatchRequest,
        tool: &str,
        declaration_version: u32,
        args: &Value,
        status: &str,
        result: Option<Value>,
        trust_rank: u8,
    ) -> Result<String, RuntimeError> {
        let mut generator = UlidGenerator::new();
        let id = CanonicalId::generate(Prefix::ToolCall, &mut generator).to_string();
        let args_digest = Digest::of_canonical_json(&canonical_json(args)).to_string();
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        sqlx::query(
            "INSERT INTO tool_calls (id, tenant_id, step_id, tool_name, args, args_digest, \
             status, result, trust_level, declaration_version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&id)
        .bind(&self.identity.tenant_id)
        .bind(&request.step_id)
        .bind(tool)
        .bind(args)
        .bind(&args_digest)
        .bind(status)
        .bind(&result)
        .bind(i16::from(trust_rank))
        .bind(i32::try_from(declaration_version).unwrap_or(1))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    async fn update_tool_call(
        &self,
        tool_call_id: &str,
        status: &str,
        effect_id: Option<&str>,
        result: Option<Value>,
    ) -> Result<(), RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        sqlx::query(
            "UPDATE tool_calls SET status = $3, effect_id = COALESCE($4, effect_id), \
             result = COALESCE($5, result), updated_at = now() \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(tool_call_id)
        .bind(&self.identity.tenant_id)
        .bind(status)
        .bind(effect_id)
        .bind(&result)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn tool_call_by_effect(
        &self,
        effect_id: &str,
    ) -> Result<Option<StoredCall>, RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        let row = sqlx::query(
            "SELECT tc.id, tc.tool_name, tc.args, tc.declaration_version, s.turn_id, t.run_id \
             FROM tool_calls tc \
             JOIN steps s ON s.id = tc.step_id \
             JOIN turns t ON t.id = s.turn_id \
             WHERE tc.effect_id = $1 AND tc.tenant_id = $2 \
             ORDER BY tc.created_at DESC LIMIT 1",
        )
        .bind(effect_id)
        .bind(&self.identity.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let run_id: String = row.try_get("run_id")?;
        let run = self
            .runs
            .load_run(&CanonicalId::parse_typed(&run_id, Prefix::Run)?)
            .await?;
        let context = self.call_context(&run).await?;
        Ok(Some(StoredCall {
            id: row.try_get("id")?,
            tool_name: row.try_get("tool_name")?,
            version: u32::try_from(row.try_get::<i32, _>("declaration_version")?).unwrap_or(1),
            args: row.try_get("args")?,
            context,
        }))
    }

    async fn emit_tool_event(
        &self,
        run: &Run,
        tool_call_id: &str,
        event: &str,
        effect_class: Option<&str>,
        payload: Value,
    ) -> Result<(), RuntimeError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id).await?;
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(aggregate_version), 0) + 1 FROM runtime_events \
             WHERE tenant_id = $1 AND aggregate_type = 'tool_call' AND aggregate_id = $2",
        )
        .bind(&self.identity.tenant_id)
        .bind(tool_call_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;

        let mut payload = payload;
        if let (Value::Object(map), Some(class)) = (&mut payload, effect_class) {
            map.insert("effect_class".to_string(), Value::String(class.to_string()));
        }
        let draft = EventDraft::new(
            "tool_call",
            tool_call_id,
            u64::try_from(next).unwrap_or(1),
            EventType::parse(event)?,
            self.identity.correlation_id,
            self.identity.actor.clone(),
        )
        .with_workspace(&run.workspace_id)
        .with_generation(run.generation)
        .with_payload(payload);
        let tenant_id = self.identity.tenant_id.clone();
        self.events
            .commit_mutation_tx(&tenant_id, move |_tx, batch| {
                Box::pin(async move {
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await?;
        Ok(())
    }
}

/// Build the approval preview an operator approves against.
///
/// The untrusted origin is present **if and only if** policy escalated the proposal, which is the
/// contract `ConsequencePreview` states: an ordinary request must not claim an untrusted origin,
/// and an escalated one must not hide it.
#[must_use]
pub fn approval_preview(
    tool: &str,
    escalated: bool,
    untrusted_refs: Vec<String>,
) -> ConsequencePreview {
    let origin = escalated.then(|| UntrustedOrigin::new(untrusted_refs));
    ConsequencePreview::new(
        Vec::new(),
        Vec::new(),
        vec![tool.to_string()],
        Vec::new(),
        origin,
    )
}

/// The stored form of a call, as the `tool_calls` row records it.
struct StoredCall {
    id: String,
    tool_name: String,
    version: u32,
    args: Value,
    context: CallContextOwned,
}

/// Owned form of [`CallContext`], which borrows the run it was built from.
struct CallContextOwned {
    workspace_id: String,
    run_id: Option<String>,
    fs_root: Option<String>,
    own_branch: Option<String>,
}

impl CallContextOwned {
    fn as_context(&self) -> CallContext<'_> {
        CallContext {
            workspace_id: &self.workspace_id,
            run_id: self.run_id.as_deref(),
            fs_root: self.fs_root.as_deref(),
            own_branch: self.own_branch.as_deref(),
        }
    }
}

/// The control-plane tools the runtime itself handles (DOMAIN.md §7.5).
pub const INTERNAL_TOOLS: [&str; 4] = [
    "user.ask",
    "work.delegate",
    "work.propose_plan",
    "memory.propose",
];

/// Whether a tool is handled by a runtime port rather than an execution host.
#[must_use]
pub fn is_internal_tool(name: &str) -> bool {
    INTERNAL_TOOLS.contains(&name)
}

fn string_arg(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(str::to_string)
}

fn string_array_arg(args: &Value, name: &str) -> Vec<String> {
    args.get(name)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn authorization_request<'a>(
    plan: &'a ToolCallPlan,
    projection: &'a CapabilityProjection,
) -> AuthorizationRequest<'a> {
    AuthorizationRequest::new(
        &plan.effect_class,
        &plan.resource,
        plan.tier,
        GrantApproval::Ask,
        &projection.inputs,
    )
}

fn tool_target(plan: &ToolCallPlan) -> String {
    format!("tool:{}", plan.tool)
}

/// The ledger's resource shape for a plan's selector.
fn effect_resource(plan: &ToolCallPlan) -> EffectResource {
    EffectResource::new(plan.resource.kind(), plan.resource.selector())
}

fn effect_refusal(error: EffectError) -> RuntimeError {
    RuntimeError::InvalidArgument(error.to_string())
}

fn failure_detail(failure: &HostFailure) -> String {
    match failure {
        HostFailure::Unavailable { owner, detail } => format!("{detail}; owned by {owner}"),
        HostFailure::Refused { detail } => detail.clone(),
    }
}

/// Bound a host result to the declaration's `max_output_bytes` before it can reach the
/// model's context (DOMAIN.md §5.6).
#[must_use]
pub fn bound_output(output: Value, max_output_bytes: usize) -> Value {
    let serialized = output.to_string();
    if serialized.len() <= max_output_bytes {
        return output;
    }
    let mut end = max_output_bytes.min(serialized.len());
    while end > 0 && !serialized.is_char_boundary(end) {
        end -= 1;
    }
    json!({
        "truncated": true,
        "bytes": end,
        "preview": &serialized[..end],
    })
}

fn decode_projection(row: &sqlx::postgres::PgRow) -> Result<CapabilityProjection, RuntimeError> {
    let id = CanonicalId::parse(&row.try_get::<String, _>("id")?)
        .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))?;
    let subject_kind = match row.try_get::<String, _>("subject_kind")?.as_str() {
        "run" => SubjectKind::Run,
        "agent_thread" => SubjectKind::AgentThread,
        "tool_call" => SubjectKind::ToolCall,
        other => {
            return Err(RuntimeError::UnknownState {
                entity: "projection_subject_kind",
                value: other.to_string(),
            })
        }
    };
    let inputs: Vec<ProjectionInput> = serde_json::from_value(row.try_get("inputs")?)?;
    let grants: Vec<Grant> = serde_json::from_value(row.try_get("grants")?)?;
    CapabilityProjection::from_parts(
        id,
        ProjectionSubject::new(subject_kind, row.try_get::<String, _>("subject_id")?),
        inputs,
        grants,
        row.try_get("computed_at")?,
        row.try_get("expires_at")?,
    )
    .map_err(|error| RuntimeError::InvalidArgument(error.to_string()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{approval_preview, bound_output, host_owner};

    #[test]
    fn output_is_bounded_to_the_declared_size() {
        let small = json!({ "ok": true });
        assert_eq!(bound_output(small.clone(), 1024), small);

        let large = json!({ "content": "x".repeat(100) });
        let bounded = bound_output(large, 32);
        assert_eq!(bounded["truncated"], json!(true));
        assert!(bounded["preview"].as_str().expect("preview").len() <= 32);
    }

    #[test]
    fn the_preview_shows_an_untrusted_origin_only_when_policy_escalated() {
        let escalated = approval_preview("web.fetch", true, vec!["tool_call:tc_1".to_string()]);
        assert!(
            escalated.shows_untrusted_origin(),
            "an escalated proposal shows its origin"
        );
        assert_eq!(escalated.targets, vec!["web.fetch".to_string()]);
        let origin = escalated.untrusted_origin.expect("origin");
        assert_eq!(origin.source_refs, vec!["tool_call:tc_1".to_string()]);

        let ordinary = approval_preview("fs.read", false, Vec::new());
        assert!(
            !ordinary.shows_untrusted_origin(),
            "an ordinary request must not claim an untrusted origin"
        );
    }

    #[test]
    fn hosts_name_their_owning_task() {
        assert_eq!(host_owner("terminal.exec"), "EXEC-006");
        assert_eq!(host_owner("browser.click"), "EXEC-009");
        assert_eq!(host_owner("computer.system_key"), "EXEC-010");
        assert_eq!(host_owner("connector.github.create_issue"), "EXEC-011");
        assert_eq!(host_owner("artifact.create"), "CORE-007");
        assert_eq!(host_owner("knowledge.cite"), "INT-006");
        assert_eq!(host_owner("scm.git.push"), "EXEC-012");
    }
}
