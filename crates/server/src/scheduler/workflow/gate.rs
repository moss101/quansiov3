//! Script gates (ADP-007, DOMAIN.md §13.8 G): the approval a script run holds before it fans out above the call threshold or
//! raises its own call budget.
//!
//! A gate is the ordinary approval path and nothing beside it: an EffectRecord of class `record.create` on the resource
//! `{kind: work, selector: "workflow:<script run>"}` (the class `work.propose_plan` and `work.delegate` settle as), an
//! ApprovalRequest bound to its params digest that parks the script run `WAITING_APPROVAL`, and, once a person granted it, the
//! single-use receipt consumed through the verifier before the effect is authorized, reserved, dispatched and settled. The
//! script cannot grant itself any of it: the decision is `ApproveEffect`'s.

use chrono::{Duration, Utc};
use quansio_capability::EffectClass;
use quansio_core::{CanonicalId, CorrelationId, Digest, Prefix, UlidGenerator};
use serde_json::json;
use sqlx::{PgPool, Row};

use super::journal::{Journal, JournalStore};
use super::{ScriptRef, WorkflowError};
use crate::control::schema;
use crate::effects::{
    EffectAuthorization, EffectLedger, EffectResource, EffectStatus, EffectTarget, NewEffect,
    TargetKind,
};
use crate::policy::{
    ApprovalRequestStatus, ApprovalRuntime, ApprovalSigner, ConsequencePreview, DispatchBinding,
    NewApprovalRequest, UntrustedOrigin,
};
use crate::runtime::state_machine::{RunStatus, RuntimeIdentity};

/// The effect class of a gate.
pub const GATE_EFFECT_CLASS: &str = "record.create";

/// How a gate stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    /// Granted and consumed: the script goes on.
    Cleared,
    /// Waiting on a person.
    Waiting,
    /// Denied, expired or superseded: the script ends.
    Denied,
}

/// The resource selector of a script run's gates.
#[must_use]
pub fn gate_selector(run_id: &str) -> String {
    format!("workflow:{run_id}")
}

/// What the gate asks, the same bytes the effect's params digest and the approval's are taken from.
fn params(script: &ScriptRef, kind: &str, calls: u64) -> serde_json::Value {
    json!({ "gate": kind, "calls": calls, "script_run": script.run_id, "routine_id": script.routine_id })
}

struct Found {
    effect_id: String,
    approval_id: Option<String>,
}

async fn find(
    pool: &PgPool,
    script: &ScriptRef,
    digest: &Digest,
) -> Result<Option<Found>, WorkflowError> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, &script.tenant_id).await?;
    let row = sqlx::query(
        "SELECT e.id AS effect_id, \
                (SELECT q.id FROM approval_requests q WHERE q.tenant_id = e.tenant_id AND q.effect_id = e.id \
                 ORDER BY q.created_at DESC LIMIT 1) AS approval_id \
         FROM effect_records e WHERE e.tenant_id = $1 AND e.run_id = $2 AND e.effect_class = $3 \
           AND e.params_digest = $4 AND e.resource ->> 'selector' = $5 \
         ORDER BY e.created_at DESC LIMIT 1",
    )
    .bind(&script.tenant_id)
    .bind(&script.run_id)
    .bind(GATE_EFFECT_CLASS)
    .bind(digest.as_str())
    .bind(gate_selector(&script.run_id))
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    row.map(|row| {
        Ok(Found {
            effect_id: row.try_get("effect_id")?,
            approval_id: row.try_get("approval_id")?,
        })
    })
    .transpose()
}

async fn projection_of(pool: &PgPool, script: &ScriptRef) -> Result<String, WorkflowError> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, &script.tenant_id).await?;
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM capability_projections WHERE tenant_id = $1 AND subject_kind = 'run' \
         AND subject_id = $2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&script.tenant_id)
    .bind(&script.run_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    id.ok_or_else(|| {
        WorkflowError::Policy(format!(
            "the script run {} has no capability projection",
            script.run_id
        ))
    })
}

fn identity(script: &ScriptRef) -> RuntimeIdentity {
    RuntimeIdentity::system(
        &script.tenant_id,
        format!("workflow:{}", script.run_id),
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

/// Make sure the gate `(kind, calls)` is cleared, creating the request when there is none and consuming the receipt once it was
/// granted. Journal entries are appended as the gate moves (`gate`, then `gate_resolved`).
///
/// # Errors
/// [`WorkflowError::Policy`] or [`WorkflowError::Effect`] for a refusal of the approval path itself; the gate stays as it was and
/// the next pass tries again.
#[allow(clippy::too_many_arguments)]
pub async fn ensure(
    pool: &PgPool,
    journal_store: &JournalStore,
    journal: &mut Journal,
    script: &ScriptRef,
    ttl_seconds: i64,
    untrusted: bool,
    kind: &str,
    calls: u64,
) -> Result<GateOutcome, WorkflowError> {
    let gate_key = format!("{kind}:{calls}");
    match journal.gate_decision(&gate_key) {
        Some("granted") => return Ok(GateOutcome::Cleared),
        Some(_) => return Ok(GateOutcome::Denied),
        None => {}
    }
    let identity = identity(script);
    let policies = crate::policy::PolicyStore::new(pool.clone(), identity.clone())
        .map_err(|error| WorkflowError::Policy(error.to_string()))?;
    let ledger = EffectLedger::new(pool.clone(), identity.clone())
        .map_err(|error| WorkflowError::Effect(error.to_string()))?;
    let runtime = ApprovalRuntime::new(pool.clone(), identity.clone())
        .map_err(|error| WorkflowError::Policy(error.to_string()))?;
    let body = params(script, kind, calls);
    let digest = Digest::of_canonical_json(&body.to_string());

    // The effect and its request, found when an earlier pass made them, made now when it did not.
    let found = find(pool, script, &digest).await?;
    let (effect_id, approval_id) = match found {
        Some(Found {
            effect_id,
            approval_id: Some(approval_id),
        }) => (effect_id, approval_id),
        other => {
            let run_id = CanonicalId::parse_typed(&script.run_id, Prefix::Run)
                .map_err(|error| WorkflowError::Policy(error.to_string()))?;
            let run = runtime
                .runtime()
                .store()
                .load_run(&run_id)
                .await
                .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
            let effect_id = match other {
                Some(found) => found.effect_id,
                None => {
                    let class = EffectClass::parse(GATE_EFFECT_CLASS)
                        .map_err(|error| WorkflowError::Effect(error.to_string()))?;
                    let tier = ledger
                        .taxonomy()
                        .tier_for(&class)
                        .map_err(|error| WorkflowError::Effect(error.to_string()))?;
                    ledger
                        .propose(NewEffect {
                            workspace_id: script.workspace_id.clone(),
                            run_id: Some(script.run_id.clone()),
                            step_id: None,
                            tool: None,
                            effect_class: class,
                            tier,
                            resource: EffectResource::new("work", gate_selector(&script.run_id)),
                            params_digest: digest.clone(),
                            capability_projection_id: projection_of(pool, script).await?,
                            // A gate is only PROPOSED here: the single-use receipt is what authorizes it.
                            authorization: EffectAuthorization::ApprovalRequired,
                            target: EffectTarget::new(TargetKind::Server, "workflow"),
                            generation: run.generation,
                        })
                        .await
                        .map_err(|error| WorkflowError::Effect(error.to_string()))?
                        .id
                }
            };
            let summary = match kind {
                "fan_out" => format!(
                    "Let the script \u{201c}{}\u{201d} make up to {calls} agent calls",
                    script.title
                ),
                _ => format!(
                    "Raise the call budget of the script \u{201c}{}\u{201d} to {calls} agent calls",
                    script.title
                ),
            };
            let preview = ConsequencePreview::new(
                Vec::new(),
                Vec::new(),
                vec![
                    format!("script run {}", script.run_id),
                    format!("up to {calls} agent calls"),
                ],
                Vec::new(),
                untrusted.then(|| UntrustedOrigin::new(vec![format!("fire:{}", script.fire_key)])),
            );
            let request = NewApprovalRequest::new(
                script.run_id.clone(),
                script.workspace_id.clone(),
                effect_id.clone(),
                Vec::new(),
                summary,
                preview,
                digest.clone(),
                projection_of(pool, script).await?,
                Utc::now() + Duration::seconds(ttl_seconds),
            );
            let parked = if run.status == RunStatus::Running {
                runtime
                    .park_for_approval(request, run.generation)
                    .await
                    .map_err(|error| WorkflowError::Policy(error.to_string()))?
                    .request
                    .id
                    .to_string()
            } else {
                // Not RUNNING: a request still needs to exist for a person to decide; the run stays as it is.
                policies
                    .create_approval_request(&request)
                    .await
                    .map_err(|error| WorkflowError::Policy(error.to_string()))?
                    .id
                    .to_string()
            };
            (effect_id, parked)
        }
    };
    if journal.gate(&gate_key).is_none() {
        journal_store
            .append(
                &script.workspace_id,
                &script.run_id,
                "gate",
                json!({
                    "gate_key": gate_key, "gate": kind, "calls": calls,
                    "effect_id": effect_id, "approval_id": approval_id, "status": "requested",
                }),
            )
            .await?;
        *journal = journal_store.load(&script.run_id).await?;
    }

    // Where the person's decision stands.
    let mut request = policies
        .load_approval_request(&approval_id)
        .await
        .map_err(|error| WorkflowError::Policy(error.to_string()))?;
    if request.status == ApprovalRequestStatus::Requested && request.expires_at <= Utc::now() {
        policies
            .expire_due(Utc::now())
            .await
            .map_err(|error| WorkflowError::Policy(error.to_string()))?;
        request = policies
            .load_approval_request(&approval_id)
            .await
            .map_err(|error| WorkflowError::Policy(error.to_string()))?;
    }
    match request.status {
        ApprovalRequestStatus::Requested => Ok(GateOutcome::Waiting),
        ApprovalRequestStatus::Granted => {
            consume(pool, &policies, &ledger, &effect_id).await?;
            journal_store
                .append(
                    &script.workspace_id,
                    &script.run_id,
                    "gate_resolved",
                    json!({ "gate_key": gate_key, "status": "granted", "effect_id": effect_id, "calls": calls }),
                )
                .await?;
            *journal = journal_store.load(&script.run_id).await?;
            Ok(GateOutcome::Cleared)
        }
        ApprovalRequestStatus::Denied
        | ApprovalRequestStatus::Expired
        | ApprovalRequestStatus::Superseded => {
            journal_store
                .append(
                    &script.workspace_id,
                    &script.run_id,
                    "gate_resolved",
                    json!({ "gate_key": gate_key, "status": "denied", "effect_id": effect_id, "calls": calls, "request": request.status.as_str() }),
                )
                .await?;
            *journal = journal_store.load(&script.run_id).await?;
            Ok(GateOutcome::Denied)
        }
    }
}

/// Consume the granted receipt and take the effect to settled, from wherever an earlier pass left it.
async fn consume(
    pool: &PgPool,
    policies: &crate::policy::PolicyStore,
    ledger: &EffectLedger,
    effect_id: &str,
) -> Result<(), WorkflowError> {
    let effect_error =
        |error: crate::effects::EffectError| WorkflowError::Effect(error.to_string());
    let mut effect = ledger.load(effect_id).await.map_err(effect_error)?;
    if effect.status == EffectStatus::Proposed {
        let signer = ApprovalSigner::from_env().ok_or_else(|| {
            WorkflowError::Policy(
                "the approval signing key (QUANSIO_APPROVAL_SIGNING_KEY) is not configured"
                    .to_string(),
            )
        })?;
        let receipt = receipt_for(pool, &effect.tenant_id, effect_id).await?;
        policies
            .verify_and_consume_receipt(
                &receipt,
                &DispatchBinding {
                    effect_id: effect_id.to_string(),
                    params_digest: effect.params_digest.clone(),
                    generation: effect.generation,
                },
                &signer,
                Utc::now(),
            )
            .await
            .map_err(|error| WorkflowError::Policy(error.to_string()))?;
        effect = ledger.load(effect_id).await.map_err(effect_error)?;
    }
    if effect.status == EffectStatus::Authorized {
        effect = ledger
            .reserve_authorized(effect_id)
            .await
            .map_err(effect_error)?;
    }
    if effect.status == EffectStatus::Reserved {
        let token = effect.dispatch_token.clone().ok_or_else(|| {
            WorkflowError::Effect("the reserved gate has no dispatch token".to_string())
        })?;
        effect = ledger
            .mark_dispatched(effect_id, &token)
            .await
            .map_err(effect_error)?;
    }
    if effect.status == EffectStatus::Dispatched {
        ledger
            .settle_success(
                effect_id,
                Some(format!("workflow-gate:{effect_id}")),
                Vec::new(),
            )
            .await
            .map_err(effect_error)?;
    }
    Ok(())
}

async fn receipt_for(
    pool: &PgPool,
    tenant_id: &str,
    effect_id: &str,
) -> Result<String, WorkflowError> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id).await?;
    let receipt: Option<String> = sqlx::query_scalar(
        "SELECT r.id FROM approval_receipts r JOIN approval_requests q ON q.id = r.request_id \
         WHERE r.effect_id = $1 AND r.tenant_id = $2 AND q.status = 'granted' \
         ORDER BY r.granted_at DESC LIMIT 1",
    )
    .bind(effect_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    receipt
        .ok_or_else(|| WorkflowError::Policy(format!("no granted receipt exists for {effect_id}")))
}

/// Whether an effect is a script gate, by its resource: the approval handlers use it to skip the model turn a resumed run would
/// otherwise take (a script run makes none).
///
/// # Errors
/// A database error.
pub async fn is_gate_effect(
    pool: &PgPool,
    tenant_id: &str,
    effect_id: &str,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let hit: Option<bool> = sqlx::query_scalar(
        "SELECT resource ->> 'selector' LIKE 'workflow:run\\_%' FROM effect_records WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(effect_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(hit.unwrap_or(false))
}
