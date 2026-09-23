//! Webhook subscriptions and the delivery ledger (DOMAIN.md §13.5).
//!
//! A subscription is the destination's `network.egress.new_destination` grant: creating one is
//! refused unless the network policy permits new destinations and the egress broker accepts
//! the URL. Its signing secret is generated here, sealed as a SecretHandle in the same
//! transaction as the row, and returned once. Every change stages a `webhook.*` event and an
//! AuditEntry in the transaction that makes it.

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, CorrelationId, OsEntropy, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use quansio_machine::egress::{blocked_host, AddressClass, Destination, PolicyRuleDecision};
use quansio_machine::secrets::{KeyProvider, NewSecret, SecretBroker};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, Row, Transaction};

use super::{
    generate_secret, network_policy_in, rfc3339, validate_event_types, WebhookError,
    SECRET_PROVIDER, WEBHOOKS_OWNER,
};

/// A subscription as the API and the stream see it: never the secret's material.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebhookSubscription {
    /// `whk_…`.
    pub id: String,
    /// The workspace it is scoped to; tenant-wide without one.
    pub workspace_id: Option<String>,
    /// The destination URL.
    pub url: String,
    /// `host:port` — the grant.
    pub destination: String,
    /// The SecretHandle holding the signing secret (an identifier, not the material).
    pub secret_handle_id: Option<String>,
    /// Event-type filter; empty selects every type.
    pub event_types: Vec<String>,
    /// `active` | `paused` | `failing`.
    pub status: String,
    /// `{last_status, last_attempt_at, last_delivered_at, consecutive_failures}`.
    pub delivery: Value,
    /// The user who created it.
    pub created_by: Option<String>,
    /// Events with a sequence after this one are delivered.
    pub start_sequence: i64,
    /// When it was created.
    pub created_at: String,
    /// When it last changed.
    pub updated_at: String,
}

/// One row of the delivery ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebhookDelivery {
    /// `whd_…`.
    pub id: String,
    /// The subscription.
    pub subscription_id: String,
    /// The event's workspace.
    pub workspace_id: Option<String>,
    /// The stable event id consumers dedupe on.
    pub event_id: String,
    /// The event type.
    pub event_type: String,
    /// The event's tenant sequence.
    pub event_sequence: i64,
    /// `pending` | `delivering` | `delivered` | `dead_lettered`.
    pub status: String,
    /// Attempts made.
    pub attempts: i32,
    /// When the next attempt is due.
    pub next_attempt_at: String,
    /// The last attempt's outcome (`http_<code>`, `timeout`, `network`, `egress_denied:<rule>`,
    /// `signing_unavailable`).
    pub last_status: Option<String>,
    /// When the last attempt was made.
    pub last_attempt_at: Option<String>,
    /// When it was delivered.
    pub delivered_at: Option<String>,
    /// When it was enqueued.
    pub created_at: String,
}

/// A subscription to create.
#[derive(Debug, Clone)]
pub struct NewSubscription {
    /// Scope; tenant-wide without one.
    pub workspace_id: Option<String>,
    /// Destination URL.
    pub url: String,
    /// Event-type filter.
    pub event_types: Vec<String>,
    /// The creating user.
    pub created_by: Option<String>,
}

/// A change to a subscription. The URL never changes: a new destination is a new grant.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionChange {
    /// `active` (also clears `failing`) or `paused`.
    pub status: Option<String>,
    /// A new filter.
    pub event_types: Option<Vec<String>>,
    /// Replace the signing secret.
    pub rotate_secret: bool,
}

const SUBSCRIPTION_COLUMNS: &str = "id, workspace_id, url, secret_handle_id, event_types, status, \
     delivery, created_by, start_sequence, created_at, updated_at";

const DELIVERY_COLUMNS: &str = "id, subscription_id, workspace_id, event_id, event_type, \
     event_sequence, status, attempts, next_attempt_at, last_status, last_attempt_at, \
     delivered_at, created_at";

type Mutation<T> = Box<
    dyn for<'a> FnOnce(
            &'a mut Transaction<'static, Postgres>,
            &'a mut EventBatch,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, WebhookError>> + Send + 'a>,
        > + Send,
>;

/// Subscriptions and deliveries of one tenant.
#[derive(Debug, Clone)]
pub struct WebhookStore {
    events: EventStore,
    tenant_id: String,
}

/// Validate a destination URL the way the egress broker will read it.
///
/// # Errors
/// [`WebhookError::Validation`] for a URL the broker cannot read or a scheme a webhook does not
/// use; [`WebhookError::EgressDenied`] for a host the broker always refuses.
pub fn validate_url(url: &str) -> Result<Destination, WebhookError> {
    if url.len() > 2048 {
        return Err(WebhookError::Validation(
            "a webhook URL is at most 2048 characters".to_string(),
        ));
    }
    let destination =
        Destination::parse(url).map_err(|denial| WebhookError::Validation(denial.to_string()))?;
    if !matches!(destination.scheme(), "https" | "http") {
        return Err(WebhookError::Validation(
            "a webhook URL is http or https".to_string(),
        ));
    }
    if let Some(why) = blocked_host(destination.host()) {
        return Err(WebhookError::EgressDenied(format!(
            "{} is never reachable: {why}",
            destination.host()
        )));
    }
    Ok(destination)
}

impl WebhookStore {
    /// A store over the pool for one tenant.
    #[must_use]
    pub fn new(pool: sqlx::PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// `CreateWebhookSubscription`: the grant, the sealed secret, the event and the audit
    /// entry, in one transaction. Returns the subscription and the secret — the only time the
    /// secret is readable.
    ///
    /// # Errors
    /// [`WebhookError::Validation`] for a malformed request, [`WebhookError::EgressDenied`] for a
    /// destination the broker refuses, [`WebhookError::PolicyDenied`] when the network policy
    /// does not permit new destinations, [`WebhookError::SigningUnavailable`] without a key
    /// provider.
    pub async fn create(
        &self,
        actor: &Actor,
        correlation: &str,
        new: NewSubscription,
        provider: Option<Arc<dyn KeyProvider>>,
    ) -> Result<(WebhookSubscription, String), WebhookError> {
        validate_event_types(&new.event_types)?;
        let destination = validate_url(&new.url)?;
        let provider = provider.ok_or(WebhookError::SigningUnavailable)?;
        let secret = generate_secret(&OsEntropy);
        let id = CanonicalId::generate(Prefix::WebhookSubscription, &mut UlidGenerator::new())
            .to_string();
        let handle_id =
            CanonicalId::generate(Prefix::SecretHandle, &mut UlidGenerator::new()).to_string();
        let tenant = self.tenant_id.clone();
        let actor = actor.clone();
        let correlation = correlation.to_string();
        let returned_secret = secret.clone();
        let subscription = self
            .commit(Box::new(move |tx, batch| {
                Box::pin(async move {
                    let workspace = new.workspace_id.as_deref();
                    grantable(tx, &tenant, workspace, &destination).await?;
                    SecretBroker::register(
                        tx,
                        provider.as_ref(),
                        &NewSecret {
                            id: &handle_id,
                            tenant_id: &tenant,
                            workspace_id: workspace,
                            provider: SECRET_PROVIDER,
                            label: &format!("signing secret of {id}"),
                            expires_at: None,
                        },
                        secret.as_bytes(),
                        &OsEntropy,
                    )
                    .await?;
                    let start: i64 = sqlx::query_scalar(
                        "SELECT COALESCE(MAX(sequence), 0) FROM runtime_events WHERE tenant_id = $1",
                    )
                    .bind(&tenant)
                    .fetch_one(&mut **tx)
                    .await?;
                    let row = sqlx::query(&format!(
                        "INSERT INTO webhook_subscriptions (id, tenant_id, workspace_id, url, \
                         secret_handle_id, event_types, status, delivery, created_by, start_sequence) \
                         VALUES ($1, $2, $3, $4, $5, $6, 'active', '{{}}'::jsonb, $7, $8) \
                         RETURNING {SUBSCRIPTION_COLUMNS}"
                    ))
                    .bind(&id)
                    .bind(&tenant)
                    .bind(workspace)
                    .bind(&new.url)
                    .bind(&handle_id)
                    .bind(json!(new.event_types))
                    .bind(new.created_by.as_deref())
                    .bind(start)
                    .fetch_one(&mut **tx)
                    .await?;
                    let subscription = subscription_from_row(&row)?;
                    stage(
                        tx,
                        batch,
                        &tenant,
                        &actor,
                        &subscription,
                        "webhook.subscription_created",
                        json!({
                            "subscription_id": subscription.id,
                            "workspace_id": subscription.workspace_id,
                            "destination": subscription.destination,
                            "event_types": subscription.event_types,
                            "status": subscription.status,
                        }),
                    )
                    .await?;
                    audit(tx, &tenant, &actor, &correlation, &subscription, "CreateWebhookSubscription", &format!("grant {}", subscription.destination)).await?;
                    Ok(subscription)
                })
            }))
            .await?;
        Ok((subscription, returned_secret))
    }

    /// `UpdateWebhookSubscription`: pause, resume (which also clears `failing`), change the
    /// filter, or rotate the secret (returned once).
    ///
    /// # Errors
    /// [`WebhookError::NotFound`], [`WebhookError::Validation`], and
    /// [`WebhookError::SigningUnavailable`] for a rotation without a key provider.
    pub async fn update(
        &self,
        actor: &Actor,
        correlation: &str,
        id: &str,
        change: SubscriptionChange,
        provider: Option<Arc<dyn KeyProvider>>,
    ) -> Result<(WebhookSubscription, Option<String>), WebhookError> {
        if let Some(status) = change.status.as_deref() {
            if !matches!(status, "active" | "paused") {
                return Err(WebhookError::Validation(format!(
                    "status {status:?}: a subscription is set active or paused"
                )));
            }
        }
        if let Some(types) = &change.event_types {
            validate_event_types(types)?;
        }
        if change.status.is_none() && change.event_types.is_none() && !change.rotate_secret {
            return Err(WebhookError::Validation(
                "nothing to change: give status, event_types or secret: \"rotate\"".to_string(),
            ));
        }
        let provider = if change.rotate_secret {
            Some(provider.ok_or(WebhookError::SigningUnavailable)?)
        } else {
            None
        };
        let secret = change.rotate_secret.then(|| generate_secret(&OsEntropy));
        let returned_secret = secret.clone();
        let tenant = self.tenant_id.clone();
        let actor = actor.clone();
        let correlation = correlation.to_string();
        let id = id.to_string();
        let subscription = self
            .commit(Box::new(move |tx, batch| {
                Box::pin(async move {
                    let current = lock(tx, &tenant, &id).await?;
                    let status = change
                        .status
                        .clone()
                        .unwrap_or_else(|| current.status.clone());
                    let types = change
                        .event_types
                        .clone()
                        .unwrap_or_else(|| current.event_types.clone());
                    let row = sqlx::query(&format!(
                        "UPDATE webhook_subscriptions SET status = $3, event_types = $4 \
                         WHERE tenant_id = $1 AND id = $2 RETURNING {SUBSCRIPTION_COLUMNS}"
                    ))
                    .bind(&tenant)
                    .bind(&id)
                    .bind(&status)
                    .bind(json!(types))
                    .fetch_one(&mut **tx)
                    .await?;
                    let updated = subscription_from_row(&row)?;
                    if change.status.is_some() || change.event_types.is_some() {
                        stage(
                            tx,
                            batch,
                            &tenant,
                            &actor,
                            &updated,
                            "webhook.subscription_updated",
                            json!({
                                "subscription_id": updated.id,
                                "status": updated.status,
                                "previous_status": current.status,
                                "event_types": updated.event_types,
                            }),
                        )
                        .await?;
                    }
                    if let (Some(secret), Some(provider)) = (secret, provider) {
                        let handle = updated.secret_handle_id.clone().ok_or_else(|| {
                            WebhookError::Conflict(format!("{} has no signing secret", updated.id))
                        })?;
                        SecretBroker::rotate(
                            tx,
                            provider.as_ref(),
                            &tenant,
                            &handle,
                            secret.as_bytes(),
                            &OsEntropy,
                        )
                        .await?;
                        stage(
                            tx,
                            batch,
                            &tenant,
                            &actor,
                            &updated,
                            "webhook.secret_rotated",
                            json!({ "subscription_id": updated.id, "secret_handle_id": handle }),
                        )
                        .await?;
                    }
                    audit(
                        tx,
                        &tenant,
                        &actor,
                        &correlation,
                        &updated,
                        "UpdateWebhookSubscription",
                        &format!("status {}", updated.status),
                    )
                    .await?;
                    Ok(updated)
                })
            }))
            .await?;
        Ok((subscription, returned_secret))
    }

    /// `DeleteWebhookSubscription`: revoke the signing secret and remove the subscription with
    /// its ledger.
    ///
    /// # Errors
    /// [`WebhookError::NotFound`].
    pub async fn delete(
        &self,
        actor: &Actor,
        correlation: &str,
        id: &str,
    ) -> Result<WebhookSubscription, WebhookError> {
        let tenant = self.tenant_id.clone();
        let actor = actor.clone();
        let correlation = correlation.to_string();
        let id = id.to_string();
        self.commit(Box::new(move |tx, batch| {
            Box::pin(async move {
                let current = lock(tx, &tenant, &id).await?;
                if let Some(handle) = &current.secret_handle_id {
                    SecretBroker::revoke(tx, &tenant, handle).await?;
                }
                sqlx::query("DELETE FROM webhook_subscriptions WHERE tenant_id = $1 AND id = $2")
                    .bind(&tenant)
                    .bind(&id)
                    .execute(&mut **tx)
                    .await?;
                stage(
                    tx,
                    batch,
                    &tenant,
                    &actor,
                    &current,
                    "webhook.subscription_deleted",
                    json!({ "subscription_id": current.id, "destination": current.destination }),
                )
                .await?;
                audit(
                    tx,
                    &tenant,
                    &actor,
                    &correlation,
                    &current,
                    "DeleteWebhookSubscription",
                    "grant revoked",
                )
                .await?;
                Ok(current)
            })
        }))
        .await
    }

    /// `RedeliverWebhook`: re-queue a dead-lettered delivery with a fresh attempt budget.
    ///
    /// # Errors
    /// [`WebhookError::NotFound`], and [`WebhookError::Conflict`] unless it is dead-lettered.
    pub async fn redeliver(
        &self,
        actor: &Actor,
        correlation: &str,
        delivery_id: &str,
    ) -> Result<WebhookDelivery, WebhookError> {
        let tenant = self.tenant_id.clone();
        let actor = actor.clone();
        let correlation = correlation.to_string();
        let delivery_id = delivery_id.to_string();
        self.commit(Box::new(move |tx, batch| {
            Box::pin(async move {
                let row = sqlx::query(&format!(
                    "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries \
                     WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
                ))
                .bind(&tenant)
                .bind(&delivery_id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(|| WebhookError::NotFound(delivery_id.clone()))?;
                let delivery = delivery_from_row(&row)?;
                if delivery.status != "dead_lettered" {
                    return Err(WebhookError::Conflict(format!(
                        "{} is {}; only a dead-lettered delivery is redelivered",
                        delivery.id, delivery.status
                    )));
                }
                let row = sqlx::query(&format!(
                    "UPDATE webhook_deliveries SET status = 'pending', attempts = 0, \
                     next_attempt_at = now(), lease_until = NULL \
                     WHERE tenant_id = $1 AND id = $2 RETURNING {DELIVERY_COLUMNS}"
                ))
                .bind(&tenant)
                .bind(&delivery_id)
                .fetch_one(&mut **tx)
                .await?;
                let requeued = delivery_from_row(&row)?;
                let subscription = lock(tx, &tenant, &requeued.subscription_id).await?;
                stage(
                    tx,
                    batch,
                    &tenant,
                    &actor,
                    &subscription,
                    "webhook.delivery_redelivered",
                    json!({
                        "subscription_id": subscription.id,
                        "delivery_id": requeued.id,
                        "event_id": requeued.event_id,
                    }),
                )
                .await?;
                audit(
                    tx,
                    &tenant,
                    &actor,
                    &correlation,
                    &subscription,
                    "RedeliverWebhook",
                    &format!("delivery {}", requeued.id),
                )
                .await?;
                Ok(requeued)
            })
        }))
        .await
    }

    /// One subscription.
    ///
    /// # Errors
    /// [`WebhookError::NotFound`].
    pub async fn load(&self, id: &str) -> Result<WebhookSubscription, WebhookError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {SUBSCRIPTION_COLUMNS} FROM webhook_subscriptions WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(&self.tenant_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WebhookError::NotFound(id.to_string()))?;
        tx.commit().await?;
        subscription_from_row(&row)
    }

    /// The tenant's subscriptions (tenant-wide ones and, with `workspace_id`, that workspace's
    /// only), each with its deliveries counted by status.
    ///
    /// # Errors
    /// A database error.
    pub async fn list(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<(WebhookSubscription, Value)>, WebhookError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {SUBSCRIPTION_COLUMNS}, \
                    (SELECT COALESCE(jsonb_object_agg(status, n), '{{}}'::jsonb) FROM \
                       (SELECT status, count(*) AS n FROM webhook_deliveries d \
                        WHERE d.tenant_id = s.tenant_id AND d.subscription_id = s.id \
                        GROUP BY status) counted) AS counts \
             FROM webhook_subscriptions s WHERE tenant_id = $1 \
               AND ($2::text IS NULL OR workspace_id IS NULL OR workspace_id = $2) \
             ORDER BY created_at, id"
        ))
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter()
            .map(|row| {
                Ok((
                    subscription_from_row(row)?,
                    row.try_get::<Value, _>("counts")?,
                ))
            })
            .collect()
    }

    /// The subscription a delivery belongs to.
    ///
    /// # Errors
    /// [`WebhookError::NotFound`].
    pub async fn subscription_of_delivery(
        &self,
        delivery_id: &str,
    ) -> Result<String, WebhookError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let found: Option<String> = sqlx::query_scalar(
            "SELECT subscription_id FROM webhook_deliveries WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(delivery_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        found.ok_or_else(|| WebhookError::NotFound(delivery_id.to_string()))
    }

    /// A subscription's ledger, newest event first.
    ///
    /// # Errors
    /// A database error.
    pub async fn deliveries(
        &self,
        subscription_id: &str,
        limit: i64,
    ) -> Result<Vec<WebhookDelivery>, WebhookError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries \
             WHERE tenant_id = $1 AND subscription_id = $2 \
             ORDER BY event_sequence DESC, id LIMIT $3"
        ))
        .bind(&self.tenant_id)
        .bind(subscription_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(delivery_from_row).collect()
    }

    async fn commit<T: Send + 'static>(&self, mutation: Mutation<T>) -> Result<T, WebhookError> {
        let rejection: Arc<Mutex<Option<WebhookError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(&self.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    match mutation(tx, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: WEBHOOKS_OWNER,
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(error) => match rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                Some(error) => Err(error),
                None => Err(WebhookError::Event(error)),
            },
        }
    }
}

/// Refuse a destination the network policy does not let a grant be issued for.
async fn grantable(
    conn: &mut PgConnection,
    tenant_id: &str,
    workspace_id: Option<&str>,
    destination: &Destination,
) -> Result<(), WebhookError> {
    let policy = network_policy_in(conn, tenant_id, workspace_id)
        .await?
        .ok_or_else(|| {
            WebhookError::PolicyDenied(
                "no network.egress.new_destination rule in the tenant or workspace policy permits \
                 a new destination"
                    .to_string(),
            )
        })?;
    if policy.decision() == PolicyRuleDecision::Deny {
        return Err(WebhookError::PolicyDenied(format!(
            "the network policy {} denies new destinations",
            policy.policy_id()
        )));
    }
    // An address literal is judged now; a name is judged at every attempt, against what it
    // resolves to then.
    if let Ok(address) = destination.host().parse::<std::net::IpAddr>() {
        let class = AddressClass::of(address);
        if !policy.allows(class) {
            return Err(WebhookError::EgressDenied(format!(
                "{address} is a {class} address, which the network policy does not allow"
            )));
        }
    }
    Ok(())
}

/// Lock a subscription row for a change.
pub(super) async fn lock(
    conn: &mut PgConnection,
    tenant_id: &str,
    id: &str,
) -> Result<WebhookSubscription, WebhookError> {
    let row = sqlx::query(&format!(
        "SELECT {SUBSCRIPTION_COLUMNS} FROM webhook_subscriptions \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| WebhookError::NotFound(id.to_string()))?;
    subscription_from_row(&row)
}

/// Stage a `webhook.*` event on the subscription's aggregate.
pub(super) async fn stage(
    conn: &mut PgConnection,
    batch: &mut EventBatch,
    tenant_id: &str,
    actor: &Actor,
    subscription: &WebhookSubscription,
    event_type: &str,
    payload: Value,
) -> Result<(), WebhookError> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(aggregate_version), 0) FROM runtime_events \
         WHERE tenant_id = $1 AND aggregate_type = 'webhook' AND aggregate_id = $2",
    )
    .bind(tenant_id)
    .bind(&subscription.id)
    .fetch_one(&mut *conn)
    .await?;
    let staged = batch
        .staged_version("webhook", &subscription.id)
        .unwrap_or(0);
    let version = u64::try_from(committed).unwrap_or(0).max(staged) + 1;
    let mut draft = EventDraft::new(
        "webhook",
        subscription.id.clone(),
        version,
        EventType::parse(event_type)?,
        CorrelationId::generate(&mut UlidGenerator::new()),
        actor.clone(),
    )
    .with_payload(payload);
    if let Some(workspace) = &subscription.workspace_id {
        draft = draft.with_workspace(workspace.clone());
    }
    batch.emit(draft);
    Ok(())
}

async fn audit(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    actor: &Actor,
    correlation: &str,
    subscription: &WebhookSubscription,
    action: &str,
    reason: &str,
) -> Result<(), WebhookError> {
    crate::audit::append_in(
        tx,
        tenant_id,
        crate::audit::AuditDraft {
            workspace_id: subscription.workspace_id.clone(),
            actor: serde_json::to_value(actor).unwrap_or(Value::Null),
            action: action.to_string(),
            target_ref: Some(format!("webhook:{}", subscription.id)),
            decision: Some("applied".to_string()),
            reason: Some(reason.to_string()),
            correlation_id: Some(correlation.to_string()),
        },
    )
    .await?;
    Ok(())
}

fn instant(row: &PgRow, column: &str) -> Result<Option<String>, WebhookError> {
    Ok(row
        .try_get::<Option<DateTime<Utc>>, _>(column)?
        .map(rfc3339))
}

pub(super) fn subscription_from_row(row: &PgRow) -> Result<WebhookSubscription, WebhookError> {
    let url: String = row.try_get("url")?;
    let destination = Destination::parse(&url)
        .map(|destination| format!("{}:{}", destination.host(), destination.port()))
        .unwrap_or_default();
    let event_types: Value = row.try_get("event_types")?;
    Ok(WebhookSubscription {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        url,
        destination,
        secret_handle_id: row.try_get("secret_handle_id")?,
        event_types: event_types
            .as_array()
            .map(|types| {
                types
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        status: row.try_get("status")?,
        delivery: row.try_get("delivery")?,
        created_by: row.try_get("created_by")?,
        start_sequence: row.try_get("start_sequence")?,
        created_at: instant(row, "created_at")?.unwrap_or_default(),
        updated_at: instant(row, "updated_at")?.unwrap_or_default(),
    })
}

pub(super) fn delivery_from_row(row: &PgRow) -> Result<WebhookDelivery, WebhookError> {
    Ok(WebhookDelivery {
        id: row.try_get("id")?,
        subscription_id: row.try_get("subscription_id")?,
        workspace_id: row.try_get("workspace_id")?,
        event_id: row.try_get("event_id")?,
        event_type: row.try_get("event_type")?,
        event_sequence: row.try_get("event_sequence")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        next_attempt_at: instant(row, "next_attempt_at")?.unwrap_or_default(),
        last_status: row.try_get("last_status")?,
        last_attempt_at: instant(row, "last_attempt_at")?,
        delivered_at: instant(row, "delivered_at")?,
        created_at: instant(row, "created_at")?.unwrap_or_default(),
    })
}
