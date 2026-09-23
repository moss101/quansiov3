//! Webhook delivery attempts (DOMAIN.md §13.5).
//!
//! The deliverer leases due deliveries of `active` and `failing` subscriptions (a `paused`
//! subscription's deliveries are held), and attempts each one:
//!
//! 1. the network policy is read *now* — tenant and workspace rules, most restrictive wins — so
//!    a `deny` (the tenant-wide kill switch) or a narrowed address-class set applies to queued
//!    retries too;
//! 2. the host is resolved and the egress broker (origin `webhook`) decides against the
//!    subscription's grant with those addresses — a name that now resolves to a loopback,
//!    private or metadata address is refused;
//! 3. the signing secret is materialized at this boundary, scoped to the subscription, and the
//!    redacted envelope is signed;
//! 4. the request goes to the vetted addresses only (the client resolves nothing itself), with
//!    redirects off and a timeout.
//!
//! The outcome is recorded under the lease: delivered, retried after backoff, or — after
//! [`super::MAX_WEBHOOK_ATTEMPTS`] — dead-lettered, which marks the subscription `failing`. A
//! lease that lapsed (the instance crashed mid-attempt) is taken again by the next pass, so a
//! delivery is attempted at least once more rather than lost; consumers dedupe on `event_id`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use quansio_core::Sequence;
use quansio_events::{Actor, EventError, EventStore, RuntimeEvent};
use quansio_machine::egress::{
    Decision, Destination, EgressBroker, EgressGrant, EgressOrigin, EgressRequest,
};
use quansio_machine::secrets::{
    KeyProvider, MaterializationScope, MaterializeRequest, SecretBroker,
};
use serde_json::json;
use sqlx::Row;

use super::store::{lock, stage};
use super::{
    canonical_instant, network_policy_in, redact_payload, sign, webhook_backoff, WebhookError,
    DELIVERY_ID_HEADER, EVENT_ID_HEADER, EVENT_TYPE_HEADER, MAX_WEBHOOK_ATTEMPTS, SIGNATURE_HEADER,
    WEBHOOKS_OWNER,
};

/// How long an attempt holds its lease.
pub const WEBHOOK_LEASE_SECONDS: i64 = 60;
/// How long a destination has to answer.
pub const WEBHOOK_TIMEOUT_SECONDS: u64 = 10;
/// Attempts in flight at once per pass.
const CONCURRENCY: usize = 8;
/// The generation a subscription's grant is issued under: a subscription is never replaced in
/// place (a new destination is a new subscription), so it has one.
const GRANT_GENERATION: i64 = 1;
/// A subscription's grant lasts as long as the subscription.
const GRANT_EXPIRY: &str = "9999-12-31T23:59:59Z";

/// Resolves a destination host to the addresses the broker judges and the request uses.
#[async_trait]
pub trait HostResolver: Send + Sync {
    /// Every address `host` resolves to now; empty when it does not resolve.
    async fn resolve(&self, host: &str, port: u16) -> Vec<IpAddr>;
}

/// The system resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

#[async_trait]
impl HostResolver for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> Vec<IpAddr> {
        tokio::net::lookup_host((host, port))
            .await
            .map(|addresses| addresses.map(|address| address.ip()).collect())
            .unwrap_or_default()
    }
}

/// What a pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WebhookDeliveryReport {
    /// Attempts made.
    pub attempted: usize,
    /// Deliveries that reached their destination.
    pub delivered: usize,
    /// Deliveries scheduled for another attempt.
    pub retried: usize,
    /// Deliveries that ran out of attempts.
    pub dead_lettered: usize,
}

struct Lease {
    id: String,
    subscription_id: String,
    event_id: String,
    event_type: String,
    event_sequence: i64,
    attempts: i32,
    url: String,
    subscription_workspace: Option<String>,
    secret_handle_id: Option<String>,
    subscription_created_at: DateTime<Utc>,
}

enum Outcome {
    Delivered(String),
    Failed(String),
}

/// The deliverer for one tenant.
#[derive(Clone)]
pub struct WebhookDeliverer {
    events: EventStore,
    tenant_id: String,
    provider: Option<Arc<dyn KeyProvider>>,
    resolver: Arc<dyn HostResolver>,
    timeout: std::time::Duration,
}

impl std::fmt::Debug for WebhookDeliverer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookDeliverer")
            .field("tenant_id", &self.tenant_id)
            .field("signing", &self.provider.is_some())
            .finish_non_exhaustive()
    }
}

impl WebhookDeliverer {
    /// A deliverer for one tenant; without a key provider every attempt fails as
    /// `signing_unavailable` (an unsigned webhook is never sent).
    #[must_use]
    pub fn new(
        pool: sqlx::PgPool,
        tenant_id: impl Into<String>,
        provider: Option<Arc<dyn KeyProvider>>,
    ) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
            provider,
            resolver: Arc::new(SystemResolver),
            timeout: std::time::Duration::from_secs(WEBHOOK_TIMEOUT_SECONDS),
        }
    }

    /// Use another resolver.
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn HostResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Use another timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attempt up to `limit` due deliveries as of `now`.
    ///
    /// # Errors
    /// A database error while leasing; an attempt's own failure is its recorded outcome.
    pub async fn deliver_due(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<WebhookDeliveryReport, WebhookError> {
        let leases = self.lease(now, limit).await?;
        let mut report = WebhookDeliveryReport {
            attempted: leases.len(),
            ..WebhookDeliveryReport::default()
        };
        let results: Vec<Result<&'static str, WebhookError>> = futures_util::stream::iter(leases)
            .map(|lease| async move {
                let outcome = self.attempt(&lease, now).await;
                self.record(&lease, outcome, now).await
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        for result in results {
            match result? {
                "delivered" => report.delivered += 1,
                "dead_lettered" => report.dead_lettered += 1,
                "pending" => report.retried += 1,
                _ => {}
            }
        }
        Ok(report)
    }

    async fn lease(&self, now: DateTime<Utc>, limit: i64) -> Result<Vec<Lease>, WebhookError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        // The application clock and the database clock can disagree slightly; whichever is
        // later is "now", so a row written with the database's default is never in the future
        // of an attempt that follows it.
        let rows = sqlx::query(
            "WITH due AS ( \
                SELECT d.id FROM webhook_deliveries d \
                JOIN webhook_subscriptions s ON s.tenant_id = d.tenant_id AND s.id = d.subscription_id \
                WHERE d.tenant_id = $1 AND s.status IN ('active', 'failing') \
                  AND ((d.status = 'pending' AND d.next_attempt_at <= GREATEST($2, now())) \
                    OR (d.status = 'delivering' AND d.lease_until <= GREATEST($2, now()))) \
                ORDER BY d.event_sequence, d.id LIMIT $3 FOR UPDATE OF d SKIP LOCKED) \
             UPDATE webhook_deliveries d SET status = 'delivering', attempts = d.attempts + 1, \
                    lease_until = GREATEST($2, now()) + make_interval(secs => $4), \
                    last_attempt_at = GREATEST($2, now()) \
             FROM due WHERE d.tenant_id = $1 AND d.id = due.id \
             RETURNING d.id, d.subscription_id, d.event_id, d.event_type, d.event_sequence, d.attempts",
        )
        .bind(&self.tenant_id)
        .bind(now)
        .bind(limit.clamp(1, 256))
        .bind(WEBHOOK_LEASE_SECONDS as f64)
        .fetch_all(&mut *tx)
        .await?;
        let mut leases = Vec::with_capacity(rows.len());
        for row in &rows {
            let subscription_id: String = row.try_get("subscription_id")?;
            let subscription = sqlx::query(
                "SELECT url, workspace_id, secret_handle_id, created_at FROM webhook_subscriptions \
                 WHERE tenant_id = $1 AND id = $2",
            )
            .bind(&self.tenant_id)
            .bind(&subscription_id)
            .fetch_one(&mut *tx)
            .await?;
            leases.push(Lease {
                id: row.try_get("id")?,
                subscription_id,
                event_id: row.try_get("event_id")?,
                event_type: row.try_get("event_type")?,
                event_sequence: row.try_get("event_sequence")?,
                attempts: row.try_get("attempts")?,
                url: subscription.try_get("url")?,
                subscription_workspace: subscription.try_get("workspace_id")?,
                secret_handle_id: subscription.try_get("secret_handle_id")?,
                subscription_created_at: subscription.try_get("created_at")?,
            });
        }
        tx.commit().await?;
        Ok(leases)
    }

    async fn event(&self, lease: &Lease) -> Result<Option<RuntimeEvent>, WebhookError> {
        let after = if lease.event_sequence > 1 {
            Some(Sequence::new(lease.event_sequence - 1).map_err(|error| {
                WebhookError::Event(EventError::MalformedEventRow(error.to_string()))
            })?)
        } else {
            None
        };
        let events = self
            .events
            .read_events_after(&self.tenant_id, after, 1)
            .await?;
        Ok(events
            .into_iter()
            .next()
            .filter(|event| event.event_id.to_string() == lease.event_id))
    }

    async fn attempt(&self, lease: &Lease, now: DateTime<Utc>) -> Outcome {
        match self.try_attempt(lease, now).await {
            Ok(outcome) => outcome,
            Err(error) => {
                eprintln!(
                    "quansio-server: webhook delivery {} could not be attempted: {error}",
                    lease.id
                );
                Outcome::Failed("internal".to_string())
            }
        }
    }

    async fn try_attempt(
        &self,
        lease: &Lease,
        now: DateTime<Utc>,
    ) -> Result<Outcome, WebhookError> {
        let Some(event) = self.event(lease).await? else {
            return Ok(Outcome::Failed("event_missing".to_string()));
        };
        let workspace = event
            .workspace_id
            .clone()
            .or_else(|| lease.subscription_workspace.clone());
        let at = canonical_instant(now);

        // 1–2. Policy as it is now, then the broker against the grant and today's addresses.
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let policy = network_policy_in(&mut tx, &self.tenant_id, workspace.as_deref()).await;
        tx.commit().await?;
        let policy = match policy {
            Ok(Some(policy)) => policy,
            Ok(None) => return Ok(Outcome::Failed("egress_denied:no_network_rule".to_string())),
            Err(_) => {
                return Ok(Outcome::Failed(
                    "egress_denied:policy_rule_invalid".to_string(),
                ))
            }
        };
        let destination = match Destination::parse(&lease.url) {
            Ok(destination) => destination,
            Err(denial) => return Ok(Outcome::Failed(format!("egress_denied:{}", denial.rule()))),
        };
        let addresses = match destination.host().parse::<IpAddr>() {
            Ok(address) => vec![address],
            Err(_) => {
                self.resolver
                    .resolve(destination.host(), destination.port())
                    .await
            }
        };
        let grant = EgressGrant {
            effect_id: lease.subscription_id.clone(),
            tenant_id: self.tenant_id.clone(),
            target_id: lease.subscription_id.clone(),
            target_generation: GRANT_GENERATION,
            capability_id: None,
            policy_id: policy.policy_id().to_string(),
            policy_version: policy.version(),
            host: destination.host().to_string(),
            port: destination.port(),
            issued_at: canonical_instant(lease.subscription_created_at),
            expires_at: GRANT_EXPIRY.to_string(),
            revoked_at: None,
        };
        let request = EgressRequest {
            tenant_id: &self.tenant_id,
            target_id: &lease.subscription_id,
            target_generation: GRANT_GENERATION,
            capability_id: None,
            origin: EgressOrigin::Webhook,
            url: &lease.url,
            resolved_addresses: &addresses,
            at: &at,
            headers: &[],
            credential_ref: lease.secret_handle_id.as_deref(),
        };
        let decision =
            EgressBroker::decide(&request, &policy, &[grant]).unwrap_or_else(Decision::Deny);
        if !decision.is_allowed() {
            eprintln!(
                "quansio-server: {}",
                EgressBroker::record(&request, &decision, &at)
            );
            return Ok(Outcome::Failed(format!(
                "egress_denied:{}",
                decision.rule()
            )));
        }

        // 3. The secret, at this boundary only.
        let (Some(provider), Some(handle_id)) = (&self.provider, &lease.secret_handle_id) else {
            return Ok(Outcome::Failed("signing_unavailable".to_string()));
        };
        let body = serde_json::to_vec(&redact_payload(&event.to_json_value()))
            .map_err(|error| WebhookError::Validation(error.to_string()))?;
        let expires_at = MaterializeRequest::expiry_after(&at, 60)?;
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let materialized = SecretBroker::materialize(
            &mut tx,
            provider.as_ref(),
            &MaterializeRequest {
                tenant_id: &self.tenant_id,
                workspace_id: lease.subscription_workspace.as_deref(),
                handle_id,
                actor: "webhook-deliverer",
                effect_id: &lease.id,
                scope: MaterializationScope::Webhook {
                    subscription_id: lease.subscription_id.clone(),
                },
                expires_at: &expires_at,
                at: &at,
            },
        )
        .await;
        let signature = match materialized {
            Ok((materialization, _access)) => {
                tx.commit().await?;
                sign(
                    materialization.material.expose(),
                    Utc::now().timestamp(),
                    &body,
                )
            }
            Err(_) => return Ok(Outcome::Failed("signing_unavailable".to_string())),
        };

        // 4. To the vetted addresses only.
        let pinned: Vec<SocketAddr> = addresses
            .iter()
            .map(|address| SocketAddr::new(*address, destination.port()))
            .collect();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(self.timeout)
            .resolve_to_addrs(destination.host(), &pinned)
            .build()
            .map_err(|error| WebhookError::Validation(error.to_string()))?;
        let response = client
            .post(&lease.url)
            .header("content-type", "application/json")
            .header(EVENT_ID_HEADER, &lease.event_id)
            .header(EVENT_TYPE_HEADER, &lease.event_type)
            .header(DELIVERY_ID_HEADER, &lease.id)
            .header(SIGNATURE_HEADER, signature)
            .body(body)
            .send()
            .await;
        Ok(match response {
            Ok(response) if response.status().is_success() => {
                Outcome::Delivered(format!("http_{}", response.status().as_u16()))
            }
            Ok(response) => Outcome::Failed(format!("http_{}", response.status().as_u16())),
            Err(error) if error.is_timeout() => Outcome::Failed("timeout".to_string()),
            Err(_) => Outcome::Failed("network".to_string()),
        })
    }

    /// Record an attempt under its lease. Returns the delivery's new status, or `lease_lost`
    /// when another pass took the delivery after this lease lapsed.
    ///
    /// A plain attempt only writes the ledger. A dead letter, or a delivery that brings a
    /// `failing` subscription back, also changes the subscription and stages its `webhook.*`
    /// event in the same transaction.
    async fn record(
        &self,
        lease: &Lease,
        outcome: Outcome,
        now: DateTime<Utc>,
    ) -> Result<&'static str, WebhookError> {
        let (status, last_status) = match outcome {
            Outcome::Delivered(last) => ("delivered", last),
            Outcome::Failed(last) if lease.attempts >= MAX_WEBHOOK_ATTEMPTS => {
                ("dead_lettered", last)
            }
            Outcome::Failed(last) => ("pending", last),
        };
        let write = LedgerWrite {
            tenant_id: self.tenant_id.clone(),
            delivery_id: lease.id.clone(),
            subscription_id: lease.subscription_id.clone(),
            attempts: lease.attempts,
            status,
            last_status,
            now,
            retry_at: now + webhook_backoff(lease.attempts),
        };
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let current: Option<String> = sqlx::query_scalar(
            "SELECT status FROM webhook_subscriptions WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(&lease.subscription_id)
        .fetch_optional(&mut *tx)
        .await?;
        let changes_subscription = status == "dead_lettered"
            || (status == "delivered" && current.as_deref() == Some("failing"));
        if !changes_subscription {
            let written = write.apply(&mut tx).await?;
            tx.commit().await?;
            return Ok(if written.is_some() {
                status
            } else {
                "lease_lost"
            });
        }
        tx.rollback().await?;

        let event_id = lease.event_id.clone();
        let event_type = lease.event_type.clone();
        let rejection: Arc<std::sync::Mutex<Option<WebhookError>>> = Arc::default();
        let slot = Arc::clone(&rejection);
        let committed = self
            .events
            .commit_mutation_tx(&self.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let result: Result<&'static str, WebhookError> = async {
                        let subscription =
                            lock(tx, &write.tenant_id, &write.subscription_id).await?;
                        if write.apply(tx).await?.is_none() {
                            return Ok("lease_lost");
                        }
                        let actor = Actor::system("webhook-deliverer");
                        let next_status = match (write.status, subscription.status.as_str()) {
                            ("delivered", "failing") => Some("active"),
                            ("dead_lettered", "active") => Some("failing"),
                            _ => None,
                        };
                        if let Some(next_status) = next_status {
                            sqlx::query(
                                "UPDATE webhook_subscriptions SET status = $3 \
                                 WHERE tenant_id = $1 AND id = $2",
                            )
                            .bind(&write.tenant_id)
                            .bind(&write.subscription_id)
                            .bind(next_status)
                            .execute(&mut **tx)
                            .await?;
                            stage(
                                tx,
                                batch,
                                &write.tenant_id,
                                &actor,
                                &subscription,
                                "webhook.subscription_updated",
                                json!({
                                    "subscription_id": subscription.id,
                                    "status": next_status,
                                    "previous_status": subscription.status,
                                    "event_types": subscription.event_types,
                                }),
                            )
                            .await?;
                        }
                        if write.status == "dead_lettered" {
                            stage(
                                tx,
                                batch,
                                &write.tenant_id,
                                &actor,
                                &subscription,
                                "webhook.delivery_dead_lettered",
                                json!({
                                    "subscription_id": subscription.id,
                                    "delivery_id": write.delivery_id,
                                    "event_id": event_id,
                                    "event_type": event_type,
                                    "attempts": write.attempts,
                                    "last_status": write.last_status,
                                }),
                            )
                            .await?;
                        }
                        Ok(write.status)
                    }
                    .await;
                    result.map_err(|error| {
                        let message = error.to_string();
                        *slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                        EventError::MutationRejected {
                            owner: WEBHOOKS_OWNER,
                            message,
                        }
                    })
                })
            })
            .await;
        match committed {
            Ok(status) => Ok(status),
            // The lease was lost inside the transaction: nothing was staged, nothing written.
            Err(EventError::NoEventStaged) => Ok("lease_lost"),
            Err(error) => Err(rejection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .unwrap_or(WebhookError::Event(error))),
        }
    }
}

/// One attempt's ledger write, guarded by its lease.
struct LedgerWrite {
    tenant_id: String,
    delivery_id: String,
    subscription_id: String,
    attempts: i32,
    status: &'static str,
    last_status: String,
    now: DateTime<Utc>,
    retry_at: DateTime<Utc>,
}

impl LedgerWrite {
    /// Write the delivery and the subscription's summary. `None` when the lease was lost: the
    /// row is no longer `delivering` at this attempt count, because a later pass took it.
    async fn apply(&self, conn: &mut sqlx::PgConnection) -> Result<Option<()>, WebhookError> {
        let updated = sqlx::query(
            "UPDATE webhook_deliveries SET status = $4, last_status = $5, lease_until = NULL, \
             next_attempt_at = CASE WHEN $4 = 'pending' THEN GREATEST($6, now()) + ($7 - $6) \
                                    ELSE next_attempt_at END, \
             delivered_at = CASE WHEN $4 = 'delivered' THEN now() ELSE delivered_at END \
             WHERE tenant_id = $1 AND id = $2 AND status = 'delivering' AND attempts = $3",
        )
        .bind(&self.tenant_id)
        .bind(&self.delivery_id)
        .bind(self.attempts)
        .bind(self.status)
        .bind(&self.last_status)
        .bind(self.now)
        .bind(self.retry_at)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Ok(None);
        }
        sqlx::query(
            "UPDATE webhook_subscriptions SET delivery = delivery \
               || jsonb_build_object('last_status', $3::text, 'last_attempt_at', now()) \
               || CASE WHEN $4 THEN jsonb_build_object('last_delivered_at', now(), \
                                                         'consecutive_failures', 0) \
                       ELSE jsonb_build_object('consecutive_failures', \
                            COALESCE((delivery ->> 'consecutive_failures')::int, 0) + 1) END \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(&self.subscription_id)
        .bind(&self.last_status)
        .bind(self.status == "delivered")
        .execute(&mut *conn)
        .await?;
        Ok(Some(()))
    }
}
