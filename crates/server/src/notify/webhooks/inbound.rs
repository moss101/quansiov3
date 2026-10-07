//! Inbound hook requests: who may fire a routine, and what they said (ADP-007, DOMAIN.md §13.8 A).
//!
//! The request carries no tenant and no session. What authenticates it is the endpoint's secret: an HMAC over
//! `"<timestamp>.<body>"` (the scheme of the outbound webhooks, [`super::signature`]) under a signing secret the secret broker
//! holds, with a tolerance window on the timestamp. The order of the checks is part of the contract: nothing touches the
//! secret until the body is within its bound, an endpoint the caller cannot name is told nothing the signature check would
//! have hidden, and the comparison is constant-time.

use chrono::{DateTime, Utc};
use quansio_events::EventStore;
use quansio_machine::secrets::{
    KeyProvider, MaterializationScope, MaterializeRequest, SecretBroker,
};
use serde_json::Value;
use sqlx::{PgPool, Row};

use super::{canonical_instant, verify, SignatureError};
use crate::control::routines::config::HooksConfig;
use crate::control::routines::hooks::{
    is_endpoint_id, signature_digest, valid_delivery_id, HookLedger,
};
use crate::control::routines::{RoutineError, RoutineStore, RoutineView};
use crate::scheduler::RoutineTriggerKind;

/// The header that carries the sender's delivery id.
pub use super::DELIVERY_ID_HEADER;
/// The header that carries the signature.
pub use super::SIGNATURE_HEADER;

/// Why an inbound request was refused, before any fire was considered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundRefusal {
    /// The body is over the configured bound.
    TooLarge {
        /// The bound.
        max_bytes: usize,
    },
    /// No such endpoint (or the routine behind it is deleted, or its trigger is no longer a hook).
    NotFound,
    /// The routine is paused.
    Paused,
    /// The signature header is missing, malformed or does not match.
    BadSignature,
    /// The timestamp is outside the tolerance.
    Stale,
    /// The signature was right and the body is not a JSON object, or a header is outside its contract.
    BadRequest(String),
    /// The deployment cannot verify (no key provider, or the secret is revoked): refused, never accepted unverified.
    Unavailable,
    /// The database failed.
    Internal(String),
}

/// An authenticated delivery.
#[derive(Debug, Clone)]
pub struct Authenticated {
    /// The routine.
    pub routine: RoutineView,
    /// The body, as a JSON object.
    pub payload: Value,
    /// The sender's delivery id, or `sig:<digest>` when it sent none.
    pub delivery_key: String,
    /// The digest of the signature header.
    pub signature_digest: String,
}

impl From<sqlx::Error> for InboundRefusal {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

/// The tenant and routine an endpoint belongs to, without a tenant context (a request carries none).
///
/// # Errors
/// Returns a database error.
pub async fn resolve_endpoint(
    pool: &PgPool,
    endpoint: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    if !is_endpoint_id(endpoint) {
        return Ok(None);
    }
    let row = sqlx::query("SELECT tenant_id, routine_id FROM routine_hook_lookup($1)")
        .bind(endpoint)
        .fetch_optional(pool)
        .await?;
    row.map(|row| Ok((row.try_get("tenant_id")?, row.try_get("routine_id")?)))
        .transpose()
}

/// What the request said about itself, apart from its body.
#[derive(Debug, Clone, Copy)]
pub struct RequestHeaders<'a> {
    /// `quansio-signature`.
    pub signature: Option<&'a str>,
    /// `quansio-delivery-id`.
    pub delivery_id: Option<&'a str>,
}

/// Authenticate one inbound request for an endpoint the caller has already resolved ([`resolve_endpoint`]) and
/// rate-limited.
///
/// # Errors
/// An [`InboundRefusal`]; nothing is recorded for a refusal.
#[allow(clippy::too_many_arguments)]
pub async fn authenticate(
    pool: &PgPool,
    config: &HooksConfig,
    keys: Option<&dyn KeyProvider>,
    resolved: (String, String),
    endpoint: &str,
    headers: RequestHeaders<'_>,
    body: &[u8],
    now: DateTime<Utc>,
) -> Result<Authenticated, InboundRefusal> {
    if body.len() > config.max_body_bytes {
        return Err(InboundRefusal::TooLarge {
            max_bytes: config.max_body_bytes,
        });
    }
    let (tenant, routine_id) = resolved;
    let (signature_header, delivery_id) = (headers.signature, headers.delivery_id);
    let store = RoutineStore::new(
        pool.clone(),
        tenant.clone(),
        quansio_events::Actor::system("hook"),
    );
    let routine = match store.get(&routine_id).await {
        Ok(routine) => routine,
        Err(RoutineError::NotFound(_)) => return Err(InboundRefusal::NotFound),
        Err(error) => return Err(InboundRefusal::Internal(error.to_string())),
    };
    let is_hook = routine
        .trigger()
        .is_ok_and(|trigger| trigger.kind == RoutineTriggerKind::Webhook);
    if routine.status == "deleted"
        || !is_hook
        || routine.hook_endpoint_id.as_deref() != Some(endpoint)
    {
        return Err(InboundRefusal::NotFound);
    }
    let Some(signature) = signature_header.filter(|value| !value.trim().is_empty()) else {
        return Err(InboundRefusal::BadSignature);
    };
    let (Some(keys), Some(handle)) = (keys, routine.hook_secret_handle_id.as_deref()) else {
        return Err(InboundRefusal::Unavailable);
    };

    // The secret, at this boundary only.
    let at = canonical_instant(now);
    let expires_at = MaterializeRequest::expiry_after(&at, 30)
        .map_err(|error| InboundRefusal::Internal(error.to_string()))?;
    let events = EventStore::new(pool.clone());
    let mut tx = events
        .begin_tenant_transaction(&tenant)
        .await
        .map_err(|error| InboundRefusal::Internal(error.to_string()))?;
    let materialized = SecretBroker::materialize(
        &mut tx,
        keys,
        &MaterializeRequest {
            tenant_id: &tenant,
            workspace_id: Some(&routine.workspace_id),
            handle_id: handle,
            actor: "routine-hook",
            effect_id: &routine.id,
            scope: MaterializationScope::Tool {
                tool: format!("routine.hook:{}", routine.id),
            },
            expires_at: &expires_at,
            at: &at,
        },
    )
    .await;
    let verdict = match materialized {
        Ok((materialization, _access)) => {
            tx.commit().await?;
            verify(
                materialization.material.expose(),
                signature,
                body,
                now.timestamp(),
                config.tolerance_seconds,
            )
        }
        // A revoked, expired or unreadable secret verifies nothing.
        Err(_) => return Err(InboundRefusal::Unavailable),
    };
    match verdict {
        Ok(()) => {}
        Err(SignatureError::Stale) => return Err(InboundRefusal::Stale),
        Err(SignatureError::Malformed | SignatureError::Mismatch) => {
            return Err(InboundRefusal::BadSignature)
        }
    }

    // Authenticated from here on: a paused routine says so only to a caller who holds its secret.
    if routine.status == "paused" {
        return Err(InboundRefusal::Paused);
    }
    let payload: Value = serde_json::from_slice(body)
        .map_err(|_| InboundRefusal::BadRequest("the body is not JSON".to_string()))?;
    if !payload.is_object() {
        return Err(InboundRefusal::BadRequest(
            "the body is a JSON object".to_string(),
        ));
    }
    let digest = signature_digest(signature);
    let delivery_key = match delivery_id {
        Some(id) if valid_delivery_id(id) => id.to_string(),
        Some(_) => {
            return Err(InboundRefusal::BadRequest(format!(
                "{DELIVERY_ID_HEADER} is 1 to 128 characters of letters, digits and . _ : -"
            )))
        }
        None => format!("sig:{digest}"),
    };
    Ok(Authenticated {
        routine,
        payload,
        delivery_key,
        signature_digest: digest,
    })
}

/// The ledger an authenticated delivery is recorded in.
#[must_use]
pub fn ledger(pool: &PgPool, tenant_id: &str) -> HookLedger {
    HookLedger::new(pool.clone(), tenant_id)
}
