//! Durable policy decisions, user rules and approvals, committed with their events
//! (DOMAIN.md §7.3, §9).
//!
//! Every write here is one PostgreSQL transaction that also stages the matching
//! `policy.*` or `approval.*` RuntimeEvent through `quansio_events`, so a decision or an
//! approval state transition can never exist without its event. A failed verification
//! returns before the first write, so a denied, expired, superseded, replayed or
//! generation-stale receipt changes nothing (DOMAIN.md §7.3, DOSSIER.md §16).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_capability::{Decision, ResourceSelector, Tier, UserRuleDecision};
use quansio_core::{CanonicalId, Digest, Generation, Prefix, UlidGenerator};
use quansio_events::{EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::policy::approval::{
    ApprovalReceipt, ApprovalRequestRecord, ApprovalRequestStatus, ApprovalSigner, DispatchBinding,
    NewApprovalRequest, ReceiptScope,
};
use crate::policy::error::{ApprovalFailure, PolicyError};
use crate::policy::evaluation::PolicyOutcome;
use crate::policy::rules::{
    user_rule_decision_str, validate_user_rule, Policy, PolicyScope, PolicySet, UserRule,
};
use crate::policy::store::rows::*;
use crate::runtime::protocol_state::ProtocolStateStore;
use crate::runtime::state_machine::{Run, RunStatus, RuntimeEngine, RuntimeError, RuntimeIdentity};

/// The event families this module writes (DOMAIN.md §9.2).
pub const POLICY_OWNER_EVENTS: &[&str] = &["policy.*", "approval.*"];

/// Repository path used to attribute a rejected mutation.
const POLICY_OWNER: &str = "crates/server/src/policy";

/// Boxed future returned by a policy mutation closure.
type BoxPolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PolicyError>> + Send + 'a>>;

/// One recorded `policy_decisions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    /// The `pdc_…` decision id.
    pub id: String,
    /// The decision.
    pub decision: Decision,
    /// The reason string.
    pub reason: String,
    /// Digest of the evaluation inputs.
    pub inputs_digest: String,
}

/// A parked run and its recorded approval request.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedApproval {
    /// The recorded request.
    pub request: ApprovalRequestRecord,
    /// The run, now in `WAITING_APPROVAL`.
    pub run: Run,
}

/// The durable policy and approval store for one tenant.
#[derive(Debug, Clone)]
pub struct PolicyStore {
    events: EventStore,
    identity: RuntimeIdentity,
}

impl PolicyStore {
    /// Bind a store to one tenant and event identity.
    ///
    /// # Errors
    /// Returns [`PolicyError::Schema`] when the tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, PolicyError> {
        crate::control::schema::validate_tenant_id(&identity.tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool),
            identity,
        })
    }

    /// The tenant this store is bound to.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.identity.tenant_id
    }

    /// The PostgreSQL pool this store writes through.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        self.events.pool()
    }

    /// Run one mutation and its RuntimeEvents in a single transaction, or commit nothing.
    async fn commit<T, F>(&self, mutation: F) -> Result<T, PolicyError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxPolicyFuture<'a, T>,
    {
        let tenant_id = self.identity.tenant_id.clone();
        let rejection: Arc<Mutex<Option<PolicyError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(&tenant_id, move |tx, batch| {
                Box::pin(async move {
                    match mutation(tx, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: POLICY_OWNER,
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(EventError::MutationRejected { .. }) => Err(rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .unwrap_or_else(|| {
                    PolicyError::Event(EventError::MutationRejected {
                        owner: POLICY_OWNER,
                        message: "transaction rejected without a recorded reason".to_string(),
                    })
                })),
            Err(error) => Err(PolicyError::Event(error)),
        }
    }

    /// Load the tenant and workspace policies effective for a workspace.
    ///
    /// # Errors
    /// Returns [`PolicyError::MalformedRule`] when a stored rule set cannot be parsed;
    /// an unparseable policy must deny, never be silently dropped.
    pub async fn load_policy_set(&self, workspace_id: &str) -> Result<PolicySet, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows = sqlx::query(
            "SELECT id, scope, workspace_id, rules, question_default_ttl_seconds, \
             approval_default_ttl_seconds, max_plan_nodes, version \
             FROM policies \
             WHERE (scope = 'tenant' AND workspace_id IS NULL) \
                OR (scope = 'workspace' AND workspace_id = $1) \
             ORDER BY id ASC",
        )
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;

        let mut set = PolicySet::default();
        for row in &rows {
            let policy = policy_from_row(row)?;
            match policy.scope {
                PolicyScope::Tenant if set.tenant.is_none() => set.tenant = Some(policy),
                PolicyScope::Workspace if set.workspace.is_none() => set.workspace = Some(policy),
                PolicyScope::Tenant | PolicyScope::Workspace => {}
            }
        }
        tx.commit().await?;
        Ok(set)
    }

    /// Load the active user rules for one user and workspace.
    ///
    /// # Errors
    /// Returns [`PolicyError::MalformedRule`] when a stored rule is not canonical.
    pub async fn load_user_rules(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<Vec<UserRule>, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let rows = sqlx::query(
            "SELECT id, user_id, workspace_id, effect_class, resource, decision, expires_at \
             FROM user_rules WHERE workspace_id = $1 AND user_id = $2 ORDER BY id ASC",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let rules = rows
            .iter()
            .map(user_rule_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(rules)
    }

    /// Store a user rule after the tier-4 `always` check.
    ///
    /// A `UserRule` that sets `always` on a tier-4 effect class is refused and nothing is
    /// written (DOMAIN.md §7.1).
    ///
    /// # Errors
    /// Returns [`PolicyError::TierFourAlwaysRejected`] for the illegal rule.
    pub async fn store_user_rule(
        &self,
        rule: &UserRule,
        tier: Tier,
    ) -> Result<UserRule, PolicyError> {
        validate_user_rule(&rule.id, &rule.effect_class, rule.decision, tier)?;
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let rule = rule.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO user_rules (id, tenant_id, workspace_id, user_id, effect_class, \
                     resource, decision, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(&rule.id)
                .bind(&tenant_id)
                .bind(&rule.workspace_id)
                .bind(&rule.user_id)
                .bind(rule.effect_class.as_str())
                .bind(serde_json::to_value(&rule.resource_selector)?)
                .bind(user_rule_decision_str(rule.decision))
                .bind(rule.expires_at)
                .execute(&mut **tx)
                .await?;
                emit(
                    batch,
                    &identity,
                    "policy",
                    &rule.id,
                    1,
                    "policy.user_rule_stored",
                    Some(&rule.workspace_id),
                    None,
                    json!({
                        "rule_id": rule.id,
                        "user_id": rule.user_id,
                        "workspace_id": rule.workspace_id,
                        "effect_class": rule.effect_class.as_str(),
                        "decision": user_rule_decision_str(rule.decision),
                    }),
                )?;
                Ok(rule)
            })
        })
        .await
    }

    /// Record a `policy_decisions` row and its `policy.decision_recorded` event.
    ///
    /// # Errors
    /// Returns a database or event error; nothing is written on failure.
    pub async fn record_decision(
        &self,
        workspace_id: &str,
        run_id: Option<&str>,
        effect_id: Option<&str>,
        capability_projection_id: Option<&str>,
        outcome: &PolicyOutcome,
    ) -> Result<DecisionRow, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let row = DecisionRow {
            id: new_prefixed_id("pdc"),
            decision: outcome.decision,
            reason: outcome.reason.as_str().to_string(),
            inputs_digest: outcome.inputs_digest.as_str().to_string(),
        };
        let row_for_write = row.clone();
        let workspace = workspace_id.to_string();
        let run = run_id.map(ToString::to_string);
        let effect = effect_id.map(ToString::to_string);
        let projection = capability_projection_id.map(ToString::to_string);
        let effective_tier = outcome.effective_tier.get();
        let escalated = outcome.escalated;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO policy_decisions (id, tenant_id, run_id, effect_id, \
                     capability_projection_id, decision, reason, inputs_digest) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(&row_for_write.id)
                .bind(&tenant_id)
                .bind(run.as_deref())
                .bind(effect.as_deref())
                .bind(projection.as_deref())
                .bind(row_for_write.decision.as_str())
                .bind(&row_for_write.reason)
                .bind(&row_for_write.inputs_digest)
                .execute(&mut **tx)
                .await?;
                emit(
                    batch,
                    &identity,
                    "policy",
                    &row_for_write.id,
                    1,
                    "policy.decision_recorded",
                    Some(&workspace),
                    None,
                    json!({
                        "decision_id": row_for_write.id,
                        "run_id": run,
                        "effect_id": effect,
                        "capability_projection_id": projection,
                        "decision": row_for_write.decision.as_str(),
                        "reason": row_for_write.reason,
                        "inputs_digest": row_for_write.inputs_digest,
                        "effective_tier": effective_tier,
                        "escalated": escalated,
                    }),
                )?;
                Ok(row_for_write)
            })
        })
        .await
    }

    /// Record an approval request, atomically adding it to the run's protocol state.
    ///
    /// # Errors
    /// Returns an error when the request cannot be written; nothing is written on failure.
    pub async fn create_approval_request(
        &self,
        request: &NewApprovalRequest,
    ) -> Result<ApprovalRequestRecord, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let id = NewApprovalRequest::generate_id();
        let workspace = request.workspace_id.clone();
        let record = approval_request_record(&id, &tenant_id, request);
        let record_for_write = record.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                insert_approval_request(tx, &tenant_id, &record_for_write).await?;
                append_pending_approval(tx, &tenant_id, &record_for_write).await?;
                emit(
                    batch,
                    &identity,
                    "approval",
                    &record_for_write.id.to_string(),
                    1,
                    "approval.requested",
                    Some(&workspace),
                    None,
                    json!({
                        "approval_id": record_for_write.id.to_string(),
                        "run_id": record_for_write.run_id,
                        "effect_id": record_for_write.effect_id,
                        "status": record_for_write.status.as_str(),
                        "params_digest": record_for_write.params_digest.as_str(),
                        "capability_projection_id": record_for_write.capability_projection_id,
                        "expires_at": record_for_write.expires_at.to_rfc3339(),
                        "consequence_preview": record_for_write.consequence_preview,
                    }),
                )?;
                Ok(record_for_write)
            })
        })
        .await
    }

    /// Load one approval request.
    ///
    /// # Errors
    /// Returns [`PolicyError::ApprovalNotFound`] when the request is not visible.
    pub async fn load_approval_request(
        &self,
        request_id: &str,
    ) -> Result<ApprovalRequestRecord, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let row = sqlx::query(&format!(
            "SELECT {APPROVAL_REQUEST_COLUMNS} FROM approval_requests WHERE id = $1"
        ))
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?;
        let record = match row {
            Some(row) => approval_request_from_row(&row)?,
            None => {
                return Err(PolicyError::ApprovalNotFound {
                    id: request_id.to_string(),
                    tenant_id,
                })
            }
        };
        tx.commit().await?;
        Ok(record)
    }

    /// Supersede a pending request when the effect parameters changed (DOMAIN.md §7.3).
    ///
    /// Returns the superseded request id, or `None` when no pending request disagrees
    /// with `params_digest`. A supersession stages exactly one `approval.superseded`
    /// event; a no-op writes nothing at all.
    ///
    /// # Errors
    /// Returns a database or event error on the superseding write.
    pub async fn supersede_if_params_changed(
        &self,
        effect_id: &str,
        params_digest: &Digest,
    ) -> Result<Option<String>, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        // Read first so a no-op supersession never opens a write transaction.
        let candidate: Option<(String, String)> = {
            let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
            let row = sqlx::query(
                "SELECT id, params_digest FROM approval_requests \
                 WHERE effect_id = $1 AND status = 'requested' \
                 ORDER BY created_at ASC LIMIT 1",
            )
            .bind(effect_id)
            .fetch_optional(&mut *tx)
            .await?;
            tx.commit().await?;
            row.map(|row| Ok::<_, PolicyError>((row.try_get("id")?, row.try_get("params_digest")?)))
                .transpose()?
        };
        let Some((request_id, current)) = candidate else {
            return Ok(None);
        };
        if current == params_digest.as_str() {
            return Ok(None);
        }
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        let params = params_digest.as_str().to_string();
        let updated = request_id.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let row = sqlx::query(
                    "SELECT workspace_id, status FROM approval_requests \
                     WHERE id = $1 FOR UPDATE",
                )
                .bind(&updated)
                .fetch_optional(&mut **tx)
                .await?;
                let Some(row) = row else {
                    return Err(PolicyError::ApprovalNotFound {
                        id: updated.clone(),
                        tenant_id: tenant_id.clone(),
                    });
                };
                let status: String = row.try_get("status")?;
                if status != ApprovalRequestStatus::Requested.as_str() {
                    return Err(PolicyError::ApprovalNotPending {
                        request_id: updated.clone(),
                        status,
                    });
                }
                let workspace_id: String = row.try_get("workspace_id")?;
                sqlx::query(
                    "UPDATE approval_requests SET status = 'superseded' \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'requested'",
                )
                .bind(&updated)
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                emit(
                    batch,
                    &identity,
                    "approval",
                    &updated,
                    2,
                    "approval.superseded",
                    Some(&workspace_id),
                    None,
                    json!({
                        "approval_id": updated,
                        "effect_id": effect_id,
                        "reason": "effect parameters changed after preview",
                        "params_digest": params,
                    }),
                )?;
                Ok(())
            })
        })
        .await?;
        Ok(Some(request_id))
    }

    /// Grant a pending request and issue a signed, single-use receipt.
    ///
    /// An expired or superseded request is never granted; the function returns a typed
    /// error and writes nothing.
    ///
    /// # Errors
    /// Returns [`PolicyError::ApprovalNotPending`] for a request that cannot be granted.
    pub async fn grant_approval(
        &self,
        request_id: &str,
        approver_user_id: &str,
        generation: Generation,
        signer: &ApprovalSigner,
        now: DateTime<Utc>,
    ) -> Result<ApprovalReceipt, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let request_id = request_id.to_string();
        let approver = approver_user_id.to_string();
        let signer = signer.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let row = sqlx::query(
                    "SELECT id, workspace_id, effect_id, params_digest, status, expires_at \
                     FROM approval_requests WHERE id = $1 FOR UPDATE",
                )
                .bind(&request_id)
                .fetch_optional(&mut **tx)
                .await?;
                let Some(row) = row else {
                    return Err(PolicyError::ApprovalNotFound {
                        id: request_id.clone(),
                        tenant_id: tenant_id.clone(),
                    });
                };
                let status: String = row.try_get("status")?;
                if status != ApprovalRequestStatus::Requested.as_str() {
                    return Err(PolicyError::ApprovalNotPending {
                        request_id: request_id.clone(),
                        status,
                    });
                }
                let expires_at: DateTime<Utc> = row.try_get("expires_at")?;
                if expires_at <= now {
                    return Err(PolicyError::ApprovalNotPending {
                        request_id: request_id.clone(),
                        status: ApprovalRequestStatus::Expired.as_str().to_string(),
                    });
                }
                let workspace_id: String = row.try_get("workspace_id")?;
                let effect_id: String = row.try_get("effect_id")?;
                let params_digest: String = row.try_get("params_digest")?;

                let mut generator = UlidGenerator::new();
                let mut receipt = ApprovalReceipt {
                    id: CanonicalId::generate(Prefix::ApprovalReceipt, &mut generator),
                    request_id: request_id.clone(),
                    effect_id,
                    approver_user_id: approver,
                    params_digest: params_digest.parse::<Digest>()?,
                    scope: ReceiptScope::SingleUse,
                    generation,
                    granted_at: now,
                    expires_at,
                    signature: String::new(),
                };
                receipt.signature = signer.sign(&receipt)?;
                sqlx::query(
                    "INSERT INTO approval_receipts (id, tenant_id, request_id, effect_id, \
                     approver_user_id, params_digest, scope, generation, granted_at, expires_at, \
                     signature) VALUES ($1, $2, $3, $4, $5, $6, 'single_use', $7, $8, $9, $10)",
                )
                .bind(receipt.id.to_string())
                .bind(&tenant_id)
                .bind(&receipt.request_id)
                .bind(&receipt.effect_id)
                .bind(&receipt.approver_user_id)
                .bind(receipt.params_digest.as_str())
                .bind(receipt.generation.get() as i64)
                .bind(receipt.granted_at)
                .bind(receipt.expires_at)
                .bind(&receipt.signature)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "UPDATE approval_requests SET status = 'granted' \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'requested'",
                )
                .bind(&request_id)
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                emit(
                    batch,
                    &identity,
                    "approval",
                    &request_id,
                    2,
                    "approval.granted",
                    Some(&workspace_id),
                    Some(generation),
                    json!({
                        "approval_id": request_id,
                        "receipt_id": receipt.id.to_string(),
                        "effect_id": receipt.effect_id,
                        "approver_user_id": receipt.approver_user_id,
                        "params_digest": receipt.params_digest.as_str(),
                        "generation": receipt.generation.get(),
                        "expires_at": receipt.expires_at.to_rfc3339(),
                    }),
                )?;
                Ok(receipt)
            })
        })
        .await
    }

    /// Deny a pending request with exactly one `approval.denied` event.
    ///
    /// # Errors
    /// Returns [`PolicyError::ApprovalNotPending`] when the request is not pending.
    pub async fn deny_approval(&self, request_id: &str) -> Result<(), PolicyError> {
        self.transition_request(request_id, ApprovalRequestStatus::Denied)
            .await
    }

    /// Mark every pending request past its expiry as `expired`, one event each.
    ///
    /// # Errors
    /// Returns a database or event error; a failed run writes nothing.
    pub async fn expire_due(&self, now: DateTime<Utc>) -> Result<Vec<String>, PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let ids: Vec<String> = {
            let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
            let rows = sqlx::query(
                "SELECT id FROM approval_requests WHERE status = 'requested' AND expires_at <= $1",
            )
            .bind(now)
            .fetch_all(&mut *tx)
            .await?;
            tx.commit().await?;
            rows.iter()
                .map(|row| row.try_get("id"))
                .collect::<Result<_, _>>()?
        };
        let mut expired = Vec::with_capacity(ids.len());
        for id in ids {
            self.transition_request(&id, ApprovalRequestStatus::Expired)
                .await?;
            expired.push(id);
        }
        Ok(expired)
    }

    async fn transition_request(
        &self,
        request_id: &str,
        to: ApprovalRequestStatus,
    ) -> Result<(), PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let request_id = request_id.to_string();
        let event_type = match to {
            ApprovalRequestStatus::Denied => "approval.denied",
            ApprovalRequestStatus::Expired => "approval.expired",
            ApprovalRequestStatus::Superseded => "approval.superseded",
            ApprovalRequestStatus::Granted | ApprovalRequestStatus::Requested => {
                return Err(PolicyError::InvalidArgument(
                    "only denied, expired and superseded are handled here".to_string(),
                ))
            }
        };
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let row = sqlx::query(
                    "SELECT workspace_id, effect_id, status FROM approval_requests \
                     WHERE id = $1 FOR UPDATE",
                )
                .bind(&request_id)
                .fetch_optional(&mut **tx)
                .await?;
                let Some(row) = row else {
                    return Err(PolicyError::ApprovalNotFound {
                        id: request_id.clone(),
                        tenant_id: tenant_id.clone(),
                    });
                };
                let status: String = row.try_get("status")?;
                if status != ApprovalRequestStatus::Requested.as_str() {
                    return Err(PolicyError::ApprovalNotPending {
                        request_id: request_id.clone(),
                        status,
                    });
                }
                let workspace_id: String = row.try_get("workspace_id")?;
                let effect_id: String = row.try_get("effect_id")?;
                sqlx::query(
                    "UPDATE approval_requests SET status = $1 \
                     WHERE id = $2 AND tenant_id = $3 AND status = 'requested'",
                )
                .bind(to.as_str())
                .bind(&request_id)
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                emit(
                    batch,
                    &identity,
                    "approval",
                    &request_id,
                    2,
                    event_type,
                    Some(&workspace_id),
                    None,
                    json!({
                        "approval_id": request_id,
                        "effect_id": effect_id,
                        "status": to.as_str(),
                    }),
                )?;
                Ok(())
            })
        })
        .await
    }

    /// Verify a receipt for a dispatch and consume its single-use scope.
    ///
    /// On any failure the function returns before the first write, so a denied,
    /// expired, superseded, replayed or stale receipt changes nothing (DOSSIER.md §16).
    ///
    /// # Errors
    /// Returns [`PolicyError::ApprovalInvalid`] with the typed failure.
    pub async fn verify_and_consume_receipt(
        &self,
        receipt_id: &str,
        binding: &DispatchBinding,
        signer: &ApprovalSigner,
        now: DateTime<Utc>,
    ) -> Result<(), PolicyError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let receipt_id = receipt_id.to_string();
        let binding = binding.clone();
        let signer = signer.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                // Read-only until every check has passed: nothing is written on failure.
                let row = sqlx::query(&format!(
                    "SELECT {RECEIPT_COLUMNS} FROM approval_receipts WHERE id = $1"
                ))
                .bind(&receipt_id)
                .fetch_optional(&mut **tx)
                .await?;
                let Some(row) = row else {
                    return Err(PolicyError::ApprovalNotFound {
                        id: receipt_id.clone(),
                        tenant_id: tenant_id.clone(),
                    });
                };
                let receipt = receipt_from_row(&row)?;
                crate::policy::approval::verify_receipt(&signer, &receipt, &binding, now)?;

                let status: Option<String> =
                    sqlx::query_scalar("SELECT status FROM approval_requests WHERE id = $1")
                        .bind(&receipt.request_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                let status = status.unwrap_or_default();
                if status != ApprovalRequestStatus::Granted.as_str() {
                    return Err(ApprovalFailure::RequestNotGranted { status }.into());
                }

                let effect = sqlx::query(
                    "SELECT workspace_id, status, params_digest, generation, approval_receipt_id \
                     FROM effect_records WHERE id = $1 FOR UPDATE",
                )
                .bind(&binding.effect_id)
                .fetch_optional(&mut **tx)
                .await?;
                let Some(effect) = effect else {
                    return Err(PolicyError::EffectNotFound {
                        effect_id: binding.effect_id.clone(),
                        tenant_id: tenant_id.clone(),
                    });
                };
                let workspace_id: String = effect.try_get("workspace_id")?;
                let effect_status: String = effect.try_get("status")?;
                let effect_params: String = effect.try_get("params_digest")?;
                let effect_generation: i64 = effect.try_get("generation")?;
                let bound_receipt: Option<String> = effect.try_get("approval_receipt_id")?;
                if bound_receipt.is_some() {
                    return Err(ApprovalFailure::AlreadyUsed.into());
                }
                let already_bound: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM effect_records WHERE approval_receipt_id = $1",
                )
                .bind(receipt.id.to_string())
                .fetch_one(&mut **tx)
                .await?;
                if already_bound > 0 {
                    return Err(ApprovalFailure::AlreadyUsed.into());
                }
                if effect_params != receipt.params_digest.as_str() {
                    return Err(ApprovalFailure::ParamsChanged {
                        expected: effect_params,
                        receipt: receipt.params_digest.as_str().to_string(),
                    }
                    .into());
                }
                if effect_generation as u64 != receipt.generation.get() {
                    return Err(ApprovalFailure::GenerationStale {
                        expected: effect_generation as u64,
                        receipt: receipt.generation.get(),
                    }
                    .into());
                }
                if !matches!(effect_status.as_str(), "PROPOSED" | "AUTHORIZED") {
                    return Err(ApprovalFailure::AlreadyUsed.into());
                }
                let updated = sqlx::query(
                    "UPDATE effect_records SET approval_receipt_id = $1, status = 'AUTHORIZED' \
                     WHERE id = $2 AND tenant_id = $3 AND approval_receipt_id IS NULL \
                       AND status IN ('PROPOSED', 'AUTHORIZED')",
                )
                .bind(receipt.id.to_string())
                .bind(&binding.effect_id)
                .bind(&tenant_id)
                .execute(&mut **tx)
                .await?;
                if updated.rows_affected() != 1 {
                    return Err(ApprovalFailure::AlreadyUsed.into());
                }
                emit(
                    batch,
                    &identity,
                    "approval",
                    &receipt.id.to_string(),
                    3,
                    "approval.consumed",
                    Some(&workspace_id),
                    Some(receipt.generation),
                    json!({
                        "receipt_id": receipt.id.to_string(),
                        "approval_id": receipt.request_id,
                        "effect_id": receipt.effect_id,
                        "params_digest": receipt.params_digest.as_str(),
                        "generation": receipt.generation.get(),
                    }),
                )?;
                Ok(())
            })
        })
        .await
    }
}

/// Parking and resuming a Run around an approval request (DOMAIN.md §5.2, §7.4).
#[derive(Clone)]
pub struct ApprovalRuntime {
    store: PolicyStore,
    runtime: RuntimeEngine,
}

impl ApprovalRuntime {
    /// Bind an approval runtime to one tenant and run engine.
    ///
    /// # Errors
    /// Returns [`PolicyError`] when the tenant identity is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, PolicyError> {
        let store = PolicyStore::new(pool.clone(), identity.clone())?;
        let runtime = RuntimeEngine::new(pool, identity).map_err(runtime_error)?;
        Ok(Self { store, runtime })
    }

    /// The durable policy store.
    #[must_use]
    pub fn store(&self) -> &PolicyStore {
        &self.store
    }

    /// The runtime engine that owns Run transitions.
    #[must_use]
    pub fn runtime(&self) -> &RuntimeEngine {
        &self.runtime
    }

    /// Park a `RUNNING` run in `WAITING_APPROVAL` with its request recorded.
    ///
    /// The request and the protocol-state entry commit together in one transaction; the
    /// canonical Run transition is applied by the runtime owner afterwards.
    ///
    /// # Errors
    /// Returns [`PolicyError::RunStateConflict`] when the run is not `RUNNING`.
    pub async fn park_for_approval(
        &self,
        request: NewApprovalRequest,
        generation: Generation,
    ) -> Result<ParkedApproval, PolicyError> {
        let run_id = CanonicalId::parse_typed(&request.run_id, Prefix::Run)?;
        let run = self
            .runtime
            .store()
            .load_run(&run_id)
            .await
            .map_err(runtime_error)?;
        if run.status != RunStatus::Running {
            return Err(PolicyError::RunStateConflict {
                run_id: request.run_id.clone(),
                status: run.status.as_db_str().to_string(),
                expected: RunStatus::Running.as_db_str().to_string(),
            });
        }
        let record = self.store.create_approval_request(&request).await?;
        let run = self
            .runtime
            .transition_run(
                &run_id,
                generation,
                RunStatus::WaitingApproval,
                Some(format!("approval {}", record.id)),
            )
            .await
            .map_err(runtime_error)?;
        Ok(ParkedApproval {
            request: record,
            run,
        })
    }

    /// Grant a pending request and release the parked run with the matching resolution.
    ///
    /// The run resumes only for this request id: `resolve_wait` removes exactly that
    /// entry from protocol state, so a receipt for another request cannot release it.
    ///
    /// # Errors
    /// Returns [`PolicyError::ApprovalNotPending`] when the request cannot be granted.
    pub async fn grant_and_resume(
        &self,
        request_id: &str,
        approver_user_id: &str,
        generation: Generation,
        signer: &ApprovalSigner,
        now: DateTime<Utc>,
    ) -> Result<ApprovalReceipt, PolicyError> {
        let request = self.store.load_approval_request(request_id).await?;
        let receipt = self
            .store
            .grant_approval(request_id, approver_user_id, generation, signer, now)
            .await?;
        let run_id = CanonicalId::parse_typed(&request.run_id, Prefix::Run)?;
        self.runtime
            .resolve_wait(
                &run_id,
                generation,
                crate::runtime::state_machine::WaitResolution::Approval {
                    approval_id: request_id.to_string(),
                },
            )
            .await
            .map_err(runtime_error)?;
        Ok(receipt)
    }
}

fn runtime_error(error: RuntimeError) -> PolicyError {
    match error {
        RuntimeError::IllegalTransition { .. }
        | RuntimeError::WaitMismatch { .. }
        | RuntimeError::StateConflict { .. }
        | RuntimeError::FencedStaleGeneration { .. } => {
            PolicyError::RuntimeConflict(error.to_string())
        }
        other => PolicyError::InvalidArgument(other.to_string()),
    }
}

/// A fresh `<prefix>_<ULID>` id for an entity without a canonical `Prefix` variant.
fn new_prefixed_id(prefix: &str) -> String {
    let mut generator = UlidGenerator::new();
    format!("{prefix}_{}", generator.generate().to_base32())
}

/// Stage one RuntimeEvent into the caller's transaction.
#[allow(clippy::too_many_arguments)]
fn emit(
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    aggregate_type: &str,
    aggregate_id: &str,
    aggregate_version: u64,
    event_type: &str,
    workspace_id: Option<&str>,
    generation: Option<Generation>,
    payload: Value,
) -> Result<(), PolicyError> {
    let parsed = EventType::parse(event_type)?;
    let mut draft = EventDraft::new(
        aggregate_type,
        aggregate_id,
        aggregate_version,
        parsed,
        identity.correlation_id,
        identity.actor.clone(),
    )
    .with_payload(payload);
    if let Some(workspace_id) = workspace_id {
        draft = draft.with_workspace(workspace_id);
    }
    if let Some(generation) = generation {
        draft = draft.with_generation(generation);
    }
    if let Some(causation_id) = &identity.causation_id {
        draft = draft.with_causation_id(causation_id.clone());
    }
    if let Some(command_id) = &identity.command_id {
        draft = draft.with_command_id(command_id.clone());
    }
    batch.emit(draft);
    Ok(())
}

/// Map a `policies` row into a typed [`Policy`].
fn policy_from_row(row: &PgRow) -> Result<Policy, PolicyError> {
    let id: String = row.try_get("id")?;
    let scope: String = row.try_get("scope")?;
    let scope = PolicyScope::parse(&scope)
        .map_err(|value| PolicyError::InvalidArgument(format!("unknown policy scope {value}")))?;
    let policy = Policy {
        id,
        scope,
        workspace_id: row.try_get("workspace_id")?,
        rules: Vec::new(),
        question_default_ttl_seconds: row.try_get("question_default_ttl_seconds")?,
        approval_default_ttl_seconds: row.try_get("approval_default_ttl_seconds")?,
        max_plan_nodes: row.try_get("max_plan_nodes")?,
        version: row.try_get("version")?,
    };
    policy.with_parsed_rules(&row.try_get::<Value, _>("rules")?)
}

/// Map a `user_rules` row into a typed [`UserRule`].
fn user_rule_from_row(row: &PgRow) -> Result<UserRule, PolicyError> {
    let id: String = row.try_get("id")?;
    let effect_class: String = row.try_get("effect_class")?;
    let decision: String = row.try_get("decision")?;
    let decision = match decision.as_str() {
        "ask" => UserRuleDecision::Ask,
        "always" => UserRuleDecision::Always,
        "never" => UserRuleDecision::Never,
        other => {
            return Err(PolicyError::MalformedRule {
                policy_id: id,
                reason: format!("unknown user-rule decision {other}"),
            })
        }
    };
    Ok(UserRule {
        id,
        user_id: row.try_get("user_id")?,
        workspace_id: row.try_get("workspace_id")?,
        effect_class: quansio_capability::EffectClass::parse(effect_class)?,
        resource_selector: serde_json::from_value::<ResourceSelector>(row.try_get("resource")?)?,
        decision,
        expires_at: row.try_get("expires_at")?,
    })
}

/// Row mapping and small approval-row helpers.
mod rows {
    use super::*;

    /// Columns selected for an `approval_requests` row.
    pub(super) const APPROVAL_REQUEST_COLUMNS: &str =
        "id, tenant_id, workspace_id, run_id, effect_id, requested_of, summary, \
         consequence_preview, params_digest, capability_projection_id, status, expires_at";

    /// Columns selected for an `approval_receipts` row.
    pub(super) const RECEIPT_COLUMNS: &str =
        "id, request_id, effect_id, approver_user_id, params_digest, scope, generation, \
         granted_at, expires_at, signature";

    /// Build the in-memory record for a request about to be inserted.
    pub(super) fn approval_request_record(
        id: &CanonicalId,
        tenant_id: &str,
        request: &NewApprovalRequest,
    ) -> ApprovalRequestRecord {
        ApprovalRequestRecord {
            id: *id,
            tenant_id: tenant_id.to_string(),
            workspace_id: request.workspace_id.clone(),
            run_id: request.run_id.clone(),
            effect_id: request.effect_id.clone(),
            requested_of: request.requested_of.clone(),
            summary: request.summary.clone(),
            consequence_preview: request.consequence_preview.clone(),
            params_digest: request.params_digest.clone(),
            capability_projection_id: request.capability_projection_id.clone(),
            status: ApprovalRequestStatus::Requested,
            expires_at: request.expires_at,
        }
    }

    /// Insert one `approval_requests` row.
    pub(super) async fn insert_approval_request(
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
        record: &ApprovalRequestRecord,
    ) -> Result<(), PolicyError> {
        sqlx::query(
            "INSERT INTO approval_requests (id, tenant_id, workspace_id, run_id, effect_id, \
             requested_of, summary, consequence_preview, params_digest, \
             capability_projection_id, status, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'requested', $11)",
        )
        .bind(record.id.to_string())
        .bind(tenant_id)
        .bind(&record.workspace_id)
        .bind(&record.run_id)
        .bind(&record.effect_id)
        .bind(serde_json::to_value(&record.requested_of)?)
        .bind(&record.summary)
        .bind(serde_json::to_value(&record.consequence_preview)?)
        .bind(record.params_digest.as_str())
        .bind(&record.capability_projection_id)
        .bind(record.expires_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Add the request to the run's durable protocol state in the same transaction.
    pub(super) async fn append_pending_approval(
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
        record: &ApprovalRequestRecord,
    ) -> Result<(), PolicyError> {
        let mut state = ProtocolStateStore::load(tx, tenant_id, &record.run_id)
            .await
            .map_err(|error| PolicyError::InvalidArgument(error.to_string()))?
            .unwrap_or_else(|| {
                crate::runtime::protocol_state::ProtocolState::new(&record.run_id, 0)
            });
        let id = record.id.to_string();
        if !state.pending_approvals.contains(&id) {
            state.pending_approvals.push(id);
        }
        ProtocolStateStore::store(tx, tenant_id, &state)
            .await
            .map_err(|error| PolicyError::InvalidArgument(error.to_string()))?;
        Ok(())
    }

    /// Map an `approval_requests` row.
    pub(super) fn approval_request_from_row(
        row: &PgRow,
    ) -> Result<ApprovalRequestRecord, PolicyError> {
        let id: String = row.try_get("id")?;
        let status: String = row.try_get("status")?;
        Ok(ApprovalRequestRecord {
            id: CanonicalId::parse(&id)?,
            tenant_id: row.try_get("tenant_id")?,
            workspace_id: row.try_get("workspace_id")?,
            run_id: row.try_get("run_id")?,
            effect_id: row.try_get("effect_id")?,
            requested_of: serde_json::from_value(row.try_get("requested_of")?)?,
            summary: row.try_get("summary")?,
            consequence_preview: serde_json::from_value(row.try_get("consequence_preview")?)?,
            params_digest: row.try_get::<String, _>("params_digest")?.parse()?,
            capability_projection_id: row.try_get("capability_projection_id")?,
            status: ApprovalRequestStatus::parse(&status)
                .map_err(|value| PolicyError::InvalidArgument(format!("unknown status {value}")))?,
            expires_at: row.try_get("expires_at")?,
        })
    }

    /// Map an `approval_receipts` row.
    pub(super) fn receipt_from_row(row: &PgRow) -> Result<ApprovalReceipt, PolicyError> {
        let id: String = row.try_get("id")?;
        let scope: String = row.try_get("scope")?;
        if scope != ReceiptScope::SingleUse.as_str() {
            return Err(PolicyError::InvalidArgument(format!(
                "unknown receipt scope {scope}"
            )));
        }
        Ok(ApprovalReceipt {
            id: CanonicalId::parse(&id)?,
            request_id: row.try_get("request_id")?,
            effect_id: row.try_get("effect_id")?,
            approver_user_id: row.try_get("approver_user_id")?,
            params_digest: row.try_get::<String, _>("params_digest")?.parse()?,
            scope: ReceiptScope::SingleUse,
            generation: Generation::new(row.try_get::<i64, _>("generation")? as u64)?,
            granted_at: row.try_get("granted_at")?,
            expires_at: row.try_get("expires_at")?,
            signature: row.try_get("signature")?,
        })
    }
}
