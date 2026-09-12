//! The Universal Effect Ledger (RUN-007, DOMAIN.md §7).
//!
//! Every consequential action has exactly one reservation/settlement path here. The
//! ledger is the sole place where a retry decision is made and the sole authority on
//! what actually happened externally:
//!
//! * a reservation takes the idempotency key derived from
//!   `(effect_class, resource, params_digest)` and is refused while any record with the
//!   same `(tenant, effect_class, key)` is in flight — enforced in code and by the
//!   partial unique index `effect_records_inflight_key_idx`;
//! * settlement is terminal, and a settled record can never be rewritten or settled
//!   again;
//! * an `OUTCOME_UNKNOWN` record is reconciled per the class strategy and can never be
//!   retried;
//! * a retry of a retryable `SETTLED_FAILED` (or a re-authorized `EXPIRED`/`DENIED`)
//!   action creates a **new** record with a new identity and the same idempotency key.
//!
//! Every state transition is one PostgreSQL transaction that writes the row change, its
//! single `effect.*` RuntimeEvent and (when the effect belongs to a tool call) the run's
//! durable protocol state, or none of them.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_capability::{EffectClass, Tier};
use quansio_core::{CanonicalId, Digest, Generation, IdempotencyKey, Prefix, UlidGenerator};
use quansio_events::{EventBatch, EventDraft, EventError, EventStore, EventType, RuntimeEvent};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::control::schema;
use crate::effects::error::EffectError;
use crate::effects::model::{
    EffectAuthorization, EffectOutcome, EffectRecord, EffectResource, EffectStatus, EffectTarget,
    NewEffect, ReconciliationEvidence, ReconciliationRecord, RetryAuthorization, TargetKind,
};
use crate::effects::taxonomy::EffectTaxonomy;
use crate::effects::EFFECTS_OWNER;
use crate::runtime::protocol_state::{PendingToolCall, ProtocolState, ProtocolStateStore};
use crate::runtime::state_machine::RuntimeIdentity;

/// Columns selected or returned for one effect record, in canonical order.
const EFFECT_COLUMNS: &str = "id, tenant_id, workspace_id, run_id, step_id, tool_call_id, \
     effect_class, tier, resource, params_digest, idempotency_key, capability_projection_id, \
     policy_decision_id, approval_receipt_id, status, dispatch_token, target_kind, target_id, \
     outcome, reconciliation, generation, reserved_at, dispatched_at, settled_at, created_at";

/// The statuses of the partial unique in-flight index (DOMAIN.md §7.2).
const IN_FLIGHT_STATUSES: &str = "'RESERVED', 'DISPATCHED', 'OUTCOME_UNKNOWN', 'RECONCILING'";

/// Boxed future returned by an effect mutation closure.
type BoxEffectFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, EffectError>> + Send + 'a>>;

/// The durable Universal Effect Ledger for one tenant.
#[derive(Debug, Clone)]
pub struct EffectLedger {
    events: EventStore,
    identity: RuntimeIdentity,
    taxonomy: &'static EffectTaxonomy,
}

impl EffectLedger {
    /// Bind a ledger to one tenant and event identity, using the shipped taxonomy.
    ///
    /// # Errors
    /// Returns [`EffectError::Schema`] when the tenant id is not canonical.
    pub fn new(pool: PgPool, identity: RuntimeIdentity) -> Result<Self, EffectError> {
        Self::with_taxonomy(pool, identity, EffectTaxonomy::builtin())
    }

    /// Bind a ledger to one tenant and an explicit taxonomy.
    ///
    /// # Errors
    /// Returns [`EffectError::Schema`] when the tenant id is not canonical.
    pub fn with_taxonomy(
        pool: PgPool,
        identity: RuntimeIdentity,
        taxonomy: &'static EffectTaxonomy,
    ) -> Result<Self, EffectError> {
        schema::validate_tenant_id(&identity.tenant_id)?;
        Ok(Self {
            events: EventStore::new(pool),
            identity,
            taxonomy,
        })
    }

    /// The tenant this ledger is bound to.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.identity.tenant_id
    }

    /// The PostgreSQL pool this ledger writes through.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        self.events.pool()
    }

    /// The taxonomy this ledger resolves effect classes against.
    #[must_use]
    pub fn taxonomy(&self) -> &'static EffectTaxonomy {
        self.taxonomy
    }

    /// Run one mutation and its RuntimeEvents in a single transaction, or commit nothing.
    async fn commit<T, F>(&self, mutation: F) -> Result<T, EffectError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxEffectFuture<'a, T>,
    {
        let tenant_id = self.identity.tenant_id.clone();
        let rejection: Arc<Mutex<Option<EffectError>>> = Arc::new(Mutex::new(None));
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
                                owner: EFFECTS_OWNER,
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
                    EffectError::Event(EventError::MutationRejected {
                        owner: EFFECTS_OWNER,
                        message: "transaction rejected without a recorded reason".to_string(),
                    })
                })),
            Err(error) => Err(EffectError::Event(error)),
        }
    }

    /// Reserve an effect and record `effect.proposed`, `effect.authorized` and
    /// `effect.reserved` (DOMAIN.md §7.2, §7.4 step "EffectRecord RESERVED").
    ///
    /// The reservation is refused while a record with the same
    /// `(tenant, effect_class, idempotency_key)` is in flight, which the partial unique
    /// index enforces even when two dispatchers race. An approval-required action must
    /// be proposed and receipt-consumed through RUN-006 first.
    ///
    /// # Errors
    /// Returns [`EffectError::DuplicateInFlight`] on a racing or repeated reservation,
    /// [`EffectError::TierMismatch`]/[`EffectError::UnknownEffectClass`] when the class
    /// disagrees with the taxonomy, and [`EffectError::ApprovalRequired`] when the
    /// action needs an approval receipt.
    pub async fn reserve(&self, new: NewEffect) -> Result<EffectRecord, EffectError> {
        let entry = self.taxonomy.entry_for(&new.effect_class)?.clone();
        if entry.tier != new.tier {
            return Err(EffectError::TierMismatch {
                effect_class: new.effect_class.to_string(),
                declared: new.tier.get(),
                expected: entry.tier.get(),
            });
        }
        let decision_id = match &new.authorization {
            EffectAuthorization::Policy { decision_id } => decision_id.clone(),
            EffectAuthorization::ApprovalRequired => {
                return Err(EffectError::ApprovalRequired(new.effect_class.to_string()));
            }
        };
        let resource = new.resource.clone();
        let key = IdempotencyKey::derive(
            new.effect_class.as_str(),
            &resource.canonical(),
            new.params_digest.clone(),
        );
        let idempotency_key = key.canonical();
        let id = new_effect_id();
        let dispatch_token = new_dispatch_token();
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let tool_name = new.tool.as_ref().map(|tool| tool.tool_name.clone());
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let sql = format!(
                    "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, step_id, \
                     tool_call_id, effect_class, tier, resource, params_digest, idempotency_key, \
                     capability_projection_id, policy_decision_id, approval_receipt_id, status, \
                     dispatch_token, target_kind, target_id, generation, reserved_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NULL, \
                     'RESERVED', $14, $15, $16, $17, now()) \
                     ON CONFLICT (tenant_id, effect_class, idempotency_key) \
                     WHERE status IN ({IN_FLIGHT_STATUSES}) DO NOTHING \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&id)
                    .bind(&tenant_id)
                    .bind(&new.workspace_id)
                    .bind(new.run_id.as_deref())
                    .bind(new.step_id.as_deref())
                    .bind(new.tool.as_ref().map(|tool| tool.tool_call_id.as_str()))
                    .bind(new.effect_class.as_str())
                    .bind(i16::from(new.tier.get()))
                    .bind(serde_json::to_value(&resource)?)
                    .bind(new.params_digest.as_str())
                    .bind(&idempotency_key)
                    .bind(&new.capability_projection_id)
                    .bind(&decision_id)
                    .bind(&dispatch_token)
                    .bind(new.target.kind.as_str())
                    .bind(&new.target.id)
                    .bind(generation_i64(new.generation)?)
                    .fetch_optional(&mut **tx)
                    .await?;
                let Some(row) = row else {
                    let existing =
                        find_in_flight(tx, &tenant_id, &new.effect_class, &idempotency_key)
                            .await?
                            .ok_or_else(|| EffectError::DuplicateInFlight {
                                effect_class: new.effect_class.to_string(),
                                idempotency_key: idempotency_key.clone(),
                                existing_effect_id: "unknown".to_string(),
                            })?;
                    return Err(EffectError::DuplicateInFlight {
                        effect_class: new.effect_class.to_string(),
                        idempotency_key,
                        existing_effect_id: existing,
                    });
                };
                let mut record = row_to_record(&row)?;
                let base = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                record.status = EffectStatus::Proposed;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    base,
                    effect_payload(&record, json!({})),
                )?;
                record.status = EffectStatus::Authorized;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    base + 1,
                    effect_payload(&record, json!({ "policy_decision_id": decision_id })),
                )?;
                record.status = EffectStatus::Reserved;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    base + 2,
                    effect_payload(
                        &record,
                        json!({ "dispatch_token": record.dispatch_token.clone() }),
                    ),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, tool_name.as_deref()).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Insert a record in `PROPOSED` for the approval flow (DOMAIN.md §7.2).
    ///
    /// RUN-006 policy then consumes the approval receipt for this effect, moving it to
    /// `AUTHORIZED`; only [`EffectLedger::reserve_authorized`] may then reserve it.
    ///
    /// # Errors
    /// Returns a taxonomy error for an unknown class or a mismatched tier.
    pub async fn propose(&self, new: NewEffect) -> Result<EffectRecord, EffectError> {
        let entry = self.taxonomy.entry_for(&new.effect_class)?.clone();
        if entry.tier != new.tier {
            return Err(EffectError::TierMismatch {
                effect_class: new.effect_class.to_string(),
                declared: new.tier.get(),
                expected: entry.tier.get(),
            });
        }
        let decision_id = match &new.authorization {
            EffectAuthorization::Policy { decision_id } => Some(decision_id.clone()),
            EffectAuthorization::ApprovalRequired => None,
        };
        let id = new_effect_id();
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let sql = format!(
                    "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, step_id, \
                     tool_call_id, effect_class, tier, resource, params_digest, idempotency_key, \
                     capability_projection_id, policy_decision_id, approval_receipt_id, status, \
                     target_kind, target_id, generation) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NULL, \
                     'PROPOSED', $14, $15, $16) RETURNING {EFFECT_COLUMNS}"
                );
                let resource = new.resource.clone();
                let key = IdempotencyKey::derive(
                    new.effect_class.as_str(),
                    &resource.canonical(),
                    new.params_digest.clone(),
                );
                let idempotency_key = key.canonical();
                let row = sqlx::query(&sql)
                    .bind(&id)
                    .bind(&tenant_id)
                    .bind(&new.workspace_id)
                    .bind(new.run_id.as_deref())
                    .bind(new.step_id.as_deref())
                    .bind(new.tool.as_ref().map(|tool| tool.tool_call_id.as_str()))
                    .bind(new.effect_class.as_str())
                    .bind(i16::from(new.tier.get()))
                    .bind(serde_json::to_value(&resource)?)
                    .bind(new.params_digest.as_str())
                    .bind(&idempotency_key)
                    .bind(&new.capability_projection_id)
                    .bind(decision_id.as_deref())
                    .bind(new.target.kind.as_str())
                    .bind(&new.target.id)
                    .bind(generation_i64(new.generation)?)
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                let base = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    base,
                    effect_payload(&record, json!({})),
                )?;
                Ok(record)
            })
        })
        .await
    }

    /// Move a proposed record to `AUTHORIZED` under a recorded policy decision.
    ///
    /// # Errors
    /// Returns [`EffectError::IllegalTransition`] unless the record is `PROPOSED`.
    pub async fn authorize(
        &self,
        effect_id: &str,
        policy_decision_id: &str,
    ) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        let decision_id = policy_decision_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                if record.status != EffectStatus::Proposed {
                    return Err(EffectError::IllegalTransition {
                        from: record.status.to_string(),
                        to: EffectStatus::Authorized.to_string(),
                    });
                }
                let sql = format!(
                    "UPDATE effect_records SET status = 'AUTHORIZED', policy_decision_id = $3 \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'PROPOSED' \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .bind(&decision_id)
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                let version = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    version,
                    effect_payload(&record, json!({ "policy_decision_id": decision_id })),
                )?;
                Ok(record)
            })
        })
        .await
    }

    /// Reserve an `AUTHORIZED` record whose approval receipt RUN-006 consumed.
    ///
    /// # Errors
    /// Returns [`EffectError::ApprovalRequired`] when no approval receipt is bound and
    /// [`EffectError::IllegalTransition`] unless the record is `AUTHORIZED`.
    pub async fn reserve_authorized(&self, effect_id: &str) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        let dispatch_token = new_dispatch_token();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                if record.status != EffectStatus::Authorized {
                    return Err(EffectError::IllegalTransition {
                        from: record.status.to_string(),
                        to: EffectStatus::Reserved.to_string(),
                    });
                }
                if record.approval_receipt_id.is_none() {
                    return Err(EffectError::ApprovalRequired(
                        record.effect_class.to_string(),
                    ));
                }
                let sql = format!(
                    "UPDATE effect_records SET status = 'RESERVED', dispatch_token = $3, \
                     reserved_at = now() WHERE id = $1 AND tenant_id = $2 AND status = 'AUTHORIZED' \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let updated = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .bind(&dispatch_token)
                    .execute(&mut **tx)
                    .await;
                let updated = match updated {
                    Ok(updated) => updated,
                    Err(error) if is_in_flight_violation(&error) => {
                        let existing = find_in_flight(
                            tx,
                            &tenant_id,
                            &record.effect_class,
                            &record.idempotency_key,
                        )
                        .await?
                        .unwrap_or_else(|| "unknown".to_string());
                        return Err(EffectError::DuplicateInFlight {
                            effect_class: record.effect_class.to_string(),
                            idempotency_key: record.idempotency_key,
                            existing_effect_id: existing,
                        });
                    }
                    Err(error) => return Err(error.into()),
                };
                if updated.rows_affected() != 1 {
                    return Err(EffectError::IllegalTransition {
                        from: record.status.to_string(),
                        to: EffectStatus::Reserved.to_string(),
                    });
                }
                let row = sqlx::query(&format!(
                    "SELECT {EFFECT_COLUMNS} FROM effect_records WHERE id = $1 AND tenant_id = $2"
                ))
                .bind(&record.id)
                .bind(&tenant_id)
                .fetch_one(&mut **tx)
                .await?;
                let record = row_to_record(&row)?;
                let version = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    version,
                    effect_payload(
                        &record,
                        json!({ "dispatch_token": record.dispatch_token.clone() }),
                    ),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, None).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Mark a reserved effect `DISPATCHED` after the host presented its dispatch token.
    ///
    /// # Errors
    /// Returns [`EffectError::IllegalTransition`] unless the record is `RESERVED`, and
    /// [`EffectError::DispatchTokenMismatch`] when the token does not match.
    pub async fn mark_dispatched(
        &self,
        effect_id: &str,
        dispatch_token: &str,
    ) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        let dispatch_token = dispatch_token.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                if record.status != EffectStatus::Reserved {
                    return Err(EffectError::IllegalTransition {
                        from: record.status.to_string(),
                        to: EffectStatus::Dispatched.to_string(),
                    });
                }
                match &record.dispatch_token {
                    Some(expected) if expected == &dispatch_token => {}
                    Some(_) => {
                        return Err(EffectError::DispatchTokenMismatch {
                            effect_id: record.id,
                        })
                    }
                    None => {
                        return Err(EffectError::MissingDispatchToken {
                            effect_id: record.id,
                        })
                    }
                }
                let sql = format!(
                    "UPDATE effect_records SET status = 'DISPATCHED', dispatched_at = now() \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'RESERVED' \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                let version = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    version,
                    effect_payload(&record, json!({})),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, None).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Settle a dispatched effect successfully; this is terminal.
    ///
    /// # Errors
    /// Returns [`EffectError::AlreadySettled`] for a terminal record and
    /// [`EffectError::IllegalTransition`] unless the record is `DISPATCHED`.
    pub async fn settle_success(
        &self,
        effect_id: &str,
        remote_ref: Option<String>,
        evidence_ids: Vec<String>,
    ) -> Result<EffectRecord, EffectError> {
        self.settle(
            effect_id,
            EffectStatus::SettledSuccess,
            EffectOutcome::success(remote_ref, evidence_ids),
        )
        .await
    }

    /// Settle a dispatched effect as failed, with an explicit retry classification.
    ///
    /// # Errors
    /// Returns [`EffectError::AlreadySettled`] for a terminal record and
    /// [`EffectError::IllegalTransition`] unless the record is `DISPATCHED`.
    pub async fn settle_failure(
        &self,
        effect_id: &str,
        retryable: bool,
        remote_ref: Option<String>,
        evidence_ids: Vec<String>,
    ) -> Result<EffectRecord, EffectError> {
        self.settle(
            effect_id,
            EffectStatus::SettledFailed,
            EffectOutcome::failure(retryable, remote_ref, evidence_ids),
        )
        .await
    }

    async fn settle(
        &self,
        effect_id: &str,
        to: EffectStatus,
        outcome: EffectOutcome,
    ) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                if record.status.is_terminal() {
                    return Err(EffectError::AlreadySettled {
                        effect_id: record.id,
                        status: record.status.to_string(),
                    });
                }
                if record.status != EffectStatus::Dispatched {
                    return Err(EffectError::IllegalTransition {
                        from: record.status.to_string(),
                        to: to.to_string(),
                    });
                }
                let sql = format!(
                    "UPDATE effect_records SET status = $3, settled_at = now(), outcome = $4 \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'DISPATCHED' \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .bind(to.as_str())
                    .bind(serde_json::to_value(&outcome)?)
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                let version = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    version,
                    effect_payload(&record, json!({ "outcome": record.outcome })),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, None).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Record that a dispatched effect's external outcome is unknown (timeout or
    /// disconnect). The record must then be reconciled, never retried.
    ///
    /// # Errors
    /// Returns [`EffectError::IllegalTransition`] unless the record is `DISPATCHED`.
    pub async fn mark_outcome_unknown(
        &self,
        effect_id: &str,
        reason: Option<String>,
    ) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                if record.status != EffectStatus::Dispatched {
                    return Err(EffectError::IllegalTransition {
                        from: record.status.to_string(),
                        to: EffectStatus::OutcomeUnknown.to_string(),
                    });
                }
                let sql = format!(
                    "UPDATE effect_records SET status = 'OUTCOME_UNKNOWN' \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'DISPATCHED' \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                let version = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    version,
                    effect_payload(&record, json!({ "reason": reason })),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, None).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Reconcile an `OUTCOME_UNKNOWN` record per its class strategy (DOMAIN.md §7.1).
    ///
    /// `Idempotent`/`Query` classes accept a deterministic [`ReconciliationEvidence::Determined`]
    /// check and settle as `RECONCILED_SUCCESS`/`RECONCILED_FAILED`; `None`/`Manual`
    /// classes accept only [`ReconciliationEvidence::Manual`] and park as
    /// `RECONCILIATION_MANUAL`. A mismatch is refused so no class is reconciled by a
    /// strategy it does not declare.
    ///
    /// # Errors
    /// Returns [`EffectError::ReconcileNotRequired`] unless the record is
    /// `OUTCOME_UNKNOWN` and [`EffectError::ReconciliationStrategyMismatch`] when the
    /// evidence does not match the class strategy.
    pub async fn reconcile(
        &self,
        effect_id: &str,
        evidence: ReconciliationEvidence,
    ) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        let taxonomy = self.taxonomy;
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                if record.status != EffectStatus::OutcomeUnknown {
                    return Err(EffectError::ReconcileNotRequired {
                        effect_id: record.id,
                        status: record.status.to_string(),
                    });
                }
                let strategy = taxonomy.strategy_for(&record.effect_class)?;
                if strategy.is_deterministic()
                    != matches!(&evidence, ReconciliationEvidence::Determined { .. })
                {
                    return Err(EffectError::ReconciliationStrategyMismatch {
                        strategy: strategy.to_string(),
                        evidence: evidence.label().to_string(),
                    });
                }
                let mut manual_evidence: Vec<String> = Vec::new();
                let (to, outcome, result) = match &evidence {
                    ReconciliationEvidence::Determined {
                        landed,
                        remote_ref,
                        evidence_ids,
                    } => {
                        if *landed {
                            (
                                EffectStatus::ReconciledSuccess,
                                Some(EffectOutcome::success(
                                    remote_ref.clone(),
                                    evidence_ids.clone(),
                                )),
                                "reconciled_success",
                            )
                        } else {
                            (
                                EffectStatus::ReconciledFailed,
                                Some(EffectOutcome::failure(
                                    false,
                                    remote_ref.clone(),
                                    evidence_ids.clone(),
                                )),
                                "reconciled_failed",
                            )
                        }
                    }
                    ReconciliationEvidence::Manual { evidence_ids } => {
                        manual_evidence = evidence_ids.clone();
                        (EffectStatus::ReconciliationManual, None, "reconciliation_manual")
                    }
                };
                let attempt = ReconciliationRecord {
                    strategy,
                    attempts: record.reconciliation.as_ref().map_or(1, |r| r.attempts + 1),
                    last_at: Utc::now(),
                    result: Some(result.to_string()),
                };
                let sql = format!(
                    "UPDATE effect_records SET status = 'RECONCILING', reconciliation = $3 \
                     WHERE id = $1 AND tenant_id = $2 AND status = 'OUTCOME_UNKNOWN' \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .bind(serde_json::to_value(&attempt)?)
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                let base = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    base,
                    effect_payload(
                        &record,
                        json!({
                            "strategy": strategy.as_str(),
                            "evidence": evidence.label(),
                            "evidence_ids": manual_evidence,
                        }),
                    ),
                )?;
                let sql = format!(
                    "UPDATE effect_records SET status = $3, settled_at = now(), outcome = $4, \
                     reconciliation = $5 WHERE id = $1 AND tenant_id = $2 AND status = 'RECONCILING' \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .bind(to.as_str())
                    .bind(match &outcome {
                        Some(outcome) => serde_json::to_value(outcome)?,
                        None => Value::Null,
                    })
                    .bind(serde_json::to_value(&attempt)?)
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    base + 1,
                    effect_payload(
                        &record,
                        json!({ "result": result, "outcome": record.outcome }),
                    ),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, None).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Record that policy denied an effect before it was dispatched.
    ///
    /// # Errors
    /// Returns [`EffectError::IllegalTransition`] unless the record is `PROPOSED`,
    /// `AUTHORIZED` or `RESERVED`.
    pub async fn deny(&self, effect_id: &str) -> Result<EffectRecord, EffectError> {
        self.terminate(effect_id, EffectStatus::Denied).await
    }

    /// Record that a pre-dispatch effect expired.
    ///
    /// # Errors
    /// Returns [`EffectError::IllegalTransition`] unless the record is `PROPOSED`,
    /// `AUTHORIZED` or `RESERVED`.
    pub async fn expire(&self, effect_id: &str) -> Result<EffectRecord, EffectError> {
        self.terminate(effect_id, EffectStatus::Expired).await
    }

    /// Record that a pre-dispatch effect was cancelled.
    ///
    /// # Errors
    /// Returns [`EffectError::IllegalTransition`] unless the record is `PROPOSED`,
    /// `AUTHORIZED` or `RESERVED`.
    pub async fn cancel(&self, effect_id: &str) -> Result<EffectRecord, EffectError> {
        self.terminate(effect_id, EffectStatus::Cancelled).await
    }

    async fn terminate(
        &self,
        effect_id: &str,
        to: EffectStatus,
    ) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                if record.status.is_terminal() {
                    return Err(EffectError::AlreadySettled {
                        effect_id: record.id,
                        status: record.status.to_string(),
                    });
                }
                if !matches!(
                    record.status,
                    EffectStatus::Proposed
                        | EffectStatus::Authorized
                        | EffectStatus::Reserved
                ) {
                    return Err(EffectError::IllegalTransition {
                        from: record.status.to_string(),
                        to: to.to_string(),
                    });
                }
                let sql = format!(
                    "UPDATE effect_records SET status = $3, settled_at = now() \
                     WHERE id = $1 AND tenant_id = $2 AND status IN ('PROPOSED', 'AUTHORIZED', 'RESERVED') \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&record.id)
                    .bind(&tenant_id)
                    .bind(to.as_str())
                    .fetch_one(&mut **tx)
                    .await?;
                let record = row_to_record(&row)?;
                let version = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    version,
                    effect_payload(&record, json!({})),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, None).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Retry a settled failure or a re-authorized `EXPIRED`/`DENIED` action.
    ///
    /// A retry **creates a new record**: new identity, new dispatch token, the same
    /// idempotency key. The old record is never mutated. An `OUTCOME_UNKNOWN`,
    /// `RECONCILING`, `RESERVED` or `DISPATCHED` record is refused: unknown outcomes are
    /// reconciled, never blindly retried (DOMAIN.md §7.2, D-014).
    ///
    /// # Errors
    /// Returns [`EffectError::RetryUnsafe`], [`EffectError::RetryNotRetryable`],
    /// [`EffectError::RetryNotAuthorized`] or [`EffectError::RetryNotAllowed`] per the
    /// record state.
    pub async fn retry(
        &self,
        effect_id: &str,
        authorization: RetryAuthorization,
    ) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let identity = self.identity.clone();
        let effect_id = effect_id.to_string();
        let new_id = new_effect_id();
        let dispatch_token = new_dispatch_token();
        self.commit(move |tx, batch| {
            Box::pin(async move {
                let record = load_for_update(tx, &tenant_id, &effect_id).await?;
                let decision_id = match record.status {
                    EffectStatus::SettledFailed => {
                        if !record
                            .outcome
                            .as_ref()
                            .is_some_and(EffectOutcome::is_retryable)
                        {
                            return Err(EffectError::RetryNotRetryable {
                                effect_id: record.id,
                            });
                        }
                        retry_decision(&record, &authorization)?
                    }
                    EffectStatus::Expired | EffectStatus::Denied => match &authorization {
                        RetryAuthorization::Reauthorized { policy_decision_id } => {
                            policy_decision_id.clone()
                        }
                        RetryAuthorization::RecordedDecision => {
                            return Err(EffectError::RetryNotAuthorized {
                                effect_id: record.id,
                                status: record.status.to_string(),
                            })
                        }
                    },
                    EffectStatus::OutcomeUnknown
                    | EffectStatus::Reconciling
                    | EffectStatus::Reserved
                    | EffectStatus::Dispatched => {
                        return Err(EffectError::RetryUnsafe {
                            effect_id: record.id,
                            status: record.status.to_string(),
                        })
                    }
                    other => {
                        return Err(EffectError::RetryNotAllowed {
                            effect_id: record.id,
                            status: other.to_string(),
                        })
                    }
                };
                let sql = format!(
                    "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, step_id, \
                     tool_call_id, effect_class, tier, resource, params_digest, idempotency_key, \
                     capability_projection_id, policy_decision_id, approval_receipt_id, status, \
                     dispatch_token, target_kind, target_id, generation, reserved_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NULL, \
                     'RESERVED', $14, $15, $16, $17, now()) \
                     ON CONFLICT (tenant_id, effect_class, idempotency_key) \
                     WHERE status IN ({IN_FLIGHT_STATUSES}) DO NOTHING \
                     RETURNING {EFFECT_COLUMNS}"
                );
                let row = sqlx::query(&sql)
                    .bind(&new_id)
                    .bind(&tenant_id)
                    .bind(&record.workspace_id)
                    .bind(record.run_id.as_deref())
                    .bind(record.step_id.as_deref())
                    .bind(record.tool_call_id.as_deref())
                    .bind(record.effect_class.as_str())
                    .bind(i16::from(record.tier.get()))
                    .bind(serde_json::to_value(&record.resource)?)
                    .bind(record.params_digest.as_str())
                    .bind(&record.idempotency_key)
                    .bind(&record.capability_projection_id)
                    .bind(&decision_id)
                    .bind(&dispatch_token)
                    .bind(record.target.as_ref().map(|target| target.kind.as_str()))
                    .bind(record.target.as_ref().map(|target| target.id.as_str()))
                    .bind(generation_i64(record.generation)?)
                    .fetch_optional(&mut **tx)
                    .await?;
                let Some(row) = row else {
                    let existing = find_in_flight(
                        tx,
                        &tenant_id,
                        &record.effect_class,
                        &record.idempotency_key,
                    )
                    .await?
                    .unwrap_or_else(|| "unknown".to_string());
                    return Err(EffectError::DuplicateInFlight {
                        effect_class: record.effect_class.to_string(),
                        idempotency_key: record.idempotency_key,
                        existing_effect_id: existing,
                    });
                };
                let record = row_to_record(&row)?;
                let version = next_aggregate_version(tx, &tenant_id, &record.id).await?;
                stage_event(
                    batch,
                    &identity,
                    &record,
                    version,
                    effect_payload(
                        &record,
                        json!({
                            "dispatch_token": record.dispatch_token.clone(),
                            "retry_of": effect_id,
                        }),
                    ),
                )?;
                sync_pending_tool_call(tx, &tenant_id, &record, None).await?;
                Ok(record)
            })
        })
        .await
    }

    /// Load one effect record, tenant-scoped.
    ///
    /// # Errors
    /// Returns [`EffectError::NotFound`] when the record is not visible in this tenant.
    pub async fn load(&self, effect_id: &str) -> Result<EffectRecord, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let sql =
            format!("SELECT {EFFECT_COLUMNS} FROM effect_records WHERE id = $1 AND tenant_id = $2");
        let row = sqlx::query(&sql)
            .bind(effect_id)
            .bind(&tenant_id)
            .fetch_optional(&mut *tx)
            .await?;
        let record = row.map(|row| row_to_record(&row)).transpose()?;
        tx.commit().await?;
        record.ok_or_else(|| EffectError::NotFound {
            id: effect_id.to_string(),
            tenant_id,
        })
    }

    /// Every unsettled effect (dispatched or awaiting reconciliation) for this tenant.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_unsettled(&self) -> Result<Vec<EffectRecord>, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let sql = format!(
            "SELECT {EFFECT_COLUMNS} FROM effect_records WHERE tenant_id = $1 \
             AND status IN ('DISPATCHED', 'OUTCOME_UNKNOWN', 'RECONCILING') \
             ORDER BY created_at ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(&tenant_id)
            .fetch_all(&mut *tx)
            .await?;
        let records = rows
            .iter()
            .map(row_to_record)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(records)
    }

    /// Every effect belonging to one run, optionally filtered by status.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_for_run(
        &self,
        run_id: &str,
        status: Option<EffectStatus>,
    ) -> Result<Vec<EffectRecord>, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let sql = format!(
            "SELECT {EFFECT_COLUMNS} FROM effect_records WHERE tenant_id = $1 AND run_id = $2 \
             AND ($3::text IS NULL OR status = $3) ORDER BY created_at ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(&tenant_id)
            .bind(run_id)
            .bind(status.map(EffectStatus::as_str))
            .fetch_all(&mut *tx)
            .await?;
        let records = rows
            .iter()
            .map(row_to_record)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(records)
    }

    /// Every record that shares one idempotency key, in creation order.
    ///
    /// This is the audit trail a retry leaves behind: the original record is unchanged
    /// and the new record carries the same key.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn find_by_key(
        &self,
        effect_class: &EffectClass,
        idempotency_key: &str,
    ) -> Result<Vec<EffectRecord>, EffectError> {
        let tenant_id = self.identity.tenant_id.clone();
        let mut tx = self.events.begin_tenant_transaction(&tenant_id).await?;
        let sql = format!(
            "SELECT {EFFECT_COLUMNS} FROM effect_records WHERE tenant_id = $1 \
             AND effect_class = $2 AND idempotency_key = $3 ORDER BY created_at ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(&tenant_id)
            .bind(effect_class.as_str())
            .bind(idempotency_key)
            .fetch_all(&mut *tx)
            .await?;
        let records = rows
            .iter()
            .map(row_to_record)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(records)
    }

    /// The ordered `effect.*` event timeline of one effect.
    ///
    /// # Errors
    /// Returns an error when the event stream cannot be read.
    pub async fn timeline(&self, effect_id: &str) -> Result<Vec<RuntimeEvent>, EffectError> {
        Ok(self
            .events
            .events_for_aggregate(&self.identity.tenant_id, "effect", effect_id)
            .await?)
    }
}

/// Resolve the policy decision a retry runs under, refusing when none is recorded.
fn retry_decision(
    record: &EffectRecord,
    authorization: &RetryAuthorization,
) -> Result<String, EffectError> {
    match authorization {
        RetryAuthorization::Reauthorized { policy_decision_id } => Ok(policy_decision_id.clone()),
        RetryAuthorization::RecordedDecision => {
            record
                .policy_decision_id
                .clone()
                .ok_or_else(|| EffectError::RetryNotAuthorized {
                    effect_id: record.id.clone(),
                    status: record.status.to_string(),
                })
        }
    }
}

/// Load one record with a row lock inside the caller's transaction.
async fn load_for_update(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    effect_id: &str,
) -> Result<EffectRecord, EffectError> {
    let sql = format!(
        "SELECT {EFFECT_COLUMNS} FROM effect_records WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(effect_id)
        .bind(tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|row| row_to_record(&row))
        .transpose()?
        .ok_or_else(|| EffectError::NotFound {
            id: effect_id.to_string(),
            tenant_id: tenant_id.to_string(),
        })
}

/// The id of the record currently holding an in-flight idempotency key, if any.
async fn find_in_flight(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    effect_class: &EffectClass,
    idempotency_key: &str,
) -> Result<Option<String>, EffectError> {
    let sql = format!(
        "SELECT id FROM effect_records WHERE tenant_id = $1 AND effect_class = $2 \
         AND idempotency_key = $3 AND status IN ({IN_FLIGHT_STATUSES}) LIMIT 1"
    );
    let id = sqlx::query_scalar::<_, String>(&sql)
        .bind(tenant_id)
        .bind(effect_class.as_str())
        .bind(idempotency_key)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(id)
}

/// The next `aggregate_version` for an effect's event stream.
async fn next_aggregate_version(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    effect_id: &str,
) -> Result<u64, EffectError> {
    let current: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(aggregate_version), 0) FROM runtime_events \
         WHERE tenant_id = $1 AND aggregate_type = 'effect' AND aggregate_id = $2",
    )
    .bind(tenant_id)
    .bind(effect_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(u64::try_from(current).unwrap_or(0) + 1)
}

/// Keep the run's durable protocol state in step with the effect (DOMAIN.md §5.7).
///
/// While the record is in flight the pending tool call carries its status, so
/// `next_safe_action` returns `ReconcileEffect` for a dispatched or unknown effect and
/// resumes only once the effect settled or was manually reconciled. A terminal record
/// removes the pending call.
async fn sync_pending_tool_call(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    record: &EffectRecord,
    tool_name: Option<&str>,
) -> Result<(), EffectError> {
    let (Some(run_id), Some(tool_call_id)) = (&record.run_id, &record.tool_call_id) else {
        return Ok(());
    };
    let mut state = ProtocolStateStore::load(tx, tenant_id, run_id)
        .await
        .map_err(|error| EffectError::ProtocolState(error.to_string()))?
        .unwrap_or_else(|| ProtocolState::new(run_id.clone(), 0));
    state.generation = generation_i64(record.generation)?;
    let existing_name = state
        .pending_tool_calls
        .iter()
        .find(|call| call.effect_id == record.id || call.tool_call_id == *tool_call_id)
        .map(|call| call.tool_name.clone());
    state
        .pending_tool_calls
        .retain(|call| call.effect_id != record.id && call.tool_call_id != *tool_call_id);
    if record.status.is_in_flight() {
        let name = match existing_name {
            Some(name) => Some(name),
            None => match tool_name {
                Some(name) => Some(name.to_string()),
                None => lookup_tool_name(tx, tenant_id, tool_call_id).await?,
            },
        }
        .unwrap_or_else(|| record.effect_class.to_string());
        state.pending_tool_calls.push(PendingToolCall {
            tool_call_id: tool_call_id.clone(),
            tool_name: name,
            dispatch_token: record.dispatch_token.clone().unwrap_or_default(),
            effect_id: record.id.clone(),
            effect_status: record.status.as_str().to_string(),
        });
    }
    ProtocolStateStore::store(tx, tenant_id, &state)
        .await
        .map_err(|error| EffectError::ProtocolState(error.to_string()))?;
    Ok(())
}

/// The registered tool name for a tool call, when the tool registry recorded one.
async fn lookup_tool_name(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    tool_call_id: &str,
) -> Result<Option<String>, EffectError> {
    let name = sqlx::query_scalar::<_, String>(
        "SELECT tool_name FROM tool_calls WHERE id = $1 AND tenant_id = $2",
    )
    .bind(tool_call_id)
    .bind(tenant_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(name)
}

/// Stage the single `effect.*` event for one transition.
fn stage_event(
    batch: &mut EventBatch,
    identity: &RuntimeIdentity,
    record: &EffectRecord,
    aggregate_version: u64,
    payload: Value,
) -> Result<(), EffectError> {
    let event_type = EventType::parse(record.status.event_name())?;
    let mut draft = EventDraft::new(
        "effect",
        &record.id,
        aggregate_version,
        event_type,
        identity.correlation_id,
        identity.actor.clone(),
    )
    .with_payload(payload)
    .with_generation(record.generation);
    draft = draft.with_workspace(&record.workspace_id);
    if let Some(causation_id) = &identity.causation_id {
        draft = draft.with_causation_id(causation_id.clone());
    }
    if let Some(command_id) = identity.command_id {
        draft = draft.with_command_id(command_id);
    }
    batch.emit(draft);
    Ok(())
}

/// The payload shared by every `effect.*` event.
fn effect_payload(record: &EffectRecord, extra: Value) -> Value {
    let mut payload = json!({
        "effect_id": record.id,
        "status": record.status.as_str(),
        "event": record.status.event_name(),
        "effect_class": record.effect_class.as_str(),
        "tier": record.tier.get(),
        "idempotency_key": record.idempotency_key,
        "workspace_id": record.workspace_id,
        "run_id": record.run_id,
        "step_id": record.step_id,
        "tool_call_id": record.tool_call_id,
        "resource": record.resource,
        "target": record.target,
        "generation": record.generation.get(),
    });
    if let (Value::Object(extra), Value::Object(payload)) = (extra, &mut payload) {
        for (key, value) in extra {
            payload.insert(key, value);
        }
    }
    payload
}

/// Whether an error is the partial unique in-flight index firing.
fn is_in_flight_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => {
            database.code().as_deref() == Some("23505")
                && database.constraint() == Some("effect_records_inflight_key_idx")
        }
        _ => false,
    }
}

/// Generate a new `eff_…` identity.
fn new_effect_id() -> String {
    let mut generator = UlidGenerator::new();
    CanonicalId::generate(Prefix::EffectRecord, &mut generator).to_string()
}

/// Generate a fresh dispatch token.
fn new_dispatch_token() -> String {
    let mut generator = UlidGenerator::new();
    format!("dtk_{}", generator.generate().to_base32())
}

/// Convert a generation to the `BIGINT` the schema stores.
fn generation_i64(generation: Generation) -> Result<i64, EffectError> {
    i64::try_from(generation.get())
        .map_err(|error| EffectError::Taxonomy(format!("generation out of range: {error}")))
}

/// Map one `effect_records` row to its typed record.
fn row_to_record(row: &PgRow) -> Result<EffectRecord, EffectError> {
    let id: String = row.try_get("id")?;
    let effect_class = EffectClass::parse(row.try_get::<String, _>("effect_class")?)?;
    let tier_raw: i16 = row.try_get("tier")?;
    let tier = u8::try_from(tier_raw)
        .map_err(|_| EffectError::UnknownStatus(format!("effect tier {tier_raw}")))
        .and_then(|value| Tier::new(value).map_err(EffectError::from))?;
    let resource: EffectResource = serde_json::from_value(row.try_get("resource")?)?;
    let params_digest: Digest = row.try_get::<String, _>("params_digest")?.parse()?;
    let status = EffectStatus::parse(&row.try_get::<String, _>("status")?)?;
    let target_kind: Option<String> = row.try_get("target_kind")?;
    let target_id: Option<String> = row.try_get("target_id")?;
    let target = match target_kind {
        Some(kind) => Some(EffectTarget {
            kind: TargetKind::parse(&kind)?,
            id: target_id.unwrap_or_default(),
        }),
        None => None,
    };
    let outcome: Option<Value> = row.try_get("outcome")?;
    let outcome = outcome
        .filter(|value| !value.is_null())
        .map(serde_json::from_value)
        .transpose()?;
    let reconciliation: Option<Value> = row.try_get("reconciliation")?;
    let reconciliation = reconciliation
        .filter(|value| !value.is_null())
        .map(serde_json::from_value)
        .transpose()?;
    let generation_raw: i64 = row.try_get("generation")?;
    let generation = u64::try_from(generation_raw)
        .map_err(|_| EffectError::UnknownStatus(format!("effect generation {generation_raw}")))
        .and_then(|value| Generation::new(value).map_err(EffectError::from))?;
    Ok(EffectRecord {
        id,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        run_id: row.try_get("run_id")?,
        step_id: row.try_get("step_id")?,
        tool_call_id: row.try_get("tool_call_id")?,
        effect_class,
        tier,
        resource,
        params_digest,
        idempotency_key: row.try_get("idempotency_key")?,
        capability_projection_id: row.try_get("capability_projection_id")?,
        policy_decision_id: row.try_get("policy_decision_id")?,
        approval_receipt_id: row.try_get("approval_receipt_id")?,
        status,
        dispatch_token: row.try_get("dispatch_token")?,
        target,
        outcome,
        reconciliation,
        generation,
        reserved_at: row.try_get::<Option<DateTime<Utc>>, _>("reserved_at")?,
        dispatched_at: row.try_get::<Option<DateTime<Utc>>, _>("dispatched_at")?,
        settled_at: row.try_get::<Option<DateTime<Utc>>, _>("settled_at")?,
        created_at: row.try_get("created_at")?,
    })
}
