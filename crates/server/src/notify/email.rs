//! Email delivery (DOMAIN.md §13.3, OD-009: server-side, never from a client).
//!
//! A notification whose channels include `email` gets a `notification_deliveries` row. The
//! delivery pass leases due rows, sends each through the configured [`EmailTransport`], and
//! records the outcome: `delivered` (and `email` joins the notification's
//! `channels_delivered`), a retry after exponential backoff, `failed` after
//! [`MAX_EMAIL_ATTEMPTS`], or `unavailable` when the deployment has no mail transport — a
//! refusal on the record, never a silent drop.
//!
//! The shipped transport posts JSON to an HTTP mail API (`QUANSIO_EMAIL_ENDPOINT`) with a
//! bearer token (`QUANSIO_EMAIL_TOKEN`), the shape transactional mail providers accept; the
//! token is read from the environment the secret broker provisions and is never logged.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use quansio_events::{EventError, EventStore};
use serde_json::json;
use sqlx::{PgPool, Row};

/// A delivery that failed this many times is marked failed.
pub const MAX_EMAIL_ATTEMPTS: i32 = 5;

/// One message to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    /// Recipient address.
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub text: String,
}

/// Sends mail.
#[async_trait]
pub trait EmailTransport: Send + Sync {
    /// Send one message.
    ///
    /// # Errors
    /// Returns a description of why the provider did not accept it.
    async fn send(&self, message: &EmailMessage) -> Result<(), String>;
}

/// An HTTP mail API: `POST <endpoint>` with `{to, subject, text}` and a bearer token.
#[derive(Debug, Clone)]
pub struct HttpEmailTransport {
    client: reqwest::Client,
    endpoint: String,
    token: Option<String>,
}

impl HttpEmailTransport {
    /// A transport for `endpoint`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            endpoint: endpoint.into(),
            token,
        }
    }

    /// The transport the environment configures, if any.
    #[must_use]
    pub fn from_env() -> Option<Arc<dyn EmailTransport>> {
        let endpoint = std::env::var("QUANSIO_EMAIL_ENDPOINT")
            .ok()
            .filter(|value| !value.is_empty())?;
        let token = std::env::var("QUANSIO_EMAIL_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        Some(Arc::new(Self::new(endpoint, token)))
    }
}

#[async_trait]
impl EmailTransport for HttpEmailTransport {
    async fn send(&self, message: &EmailMessage) -> Result<(), String> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .json(&json!({ "to": message.to, "subject": message.subject, "text": message.text }));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("the mail API answered {}", response.status()))
        }
    }
}

/// The retry delay after `attempts` failures: 30 s doubling, capped at one hour.
#[must_use]
pub fn email_backoff(attempts: i32) -> Duration {
    let exponent = u32::try_from(attempts.clamp(1, 16) - 1).unwrap_or(0);
    Duration::seconds(
        30_i64
            .saturating_mul(2_i64.saturating_pow(exponent))
            .min(3600),
    )
}

/// What one delivery pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeliveryReport {
    /// Sent.
    pub delivered: usize,
    /// Failed this time, to be retried.
    pub retried: usize,
    /// Failed for good.
    pub failed: usize,
    /// No transport configured.
    pub unavailable: usize,
}

/// The email delivery pass for one tenant.
#[derive(Clone)]
pub struct EmailDeliveries {
    events: EventStore,
    tenant_id: String,
    transport: Option<Arc<dyn EmailTransport>>,
}

impl EmailDeliveries {
    /// A pass over the pool for one tenant with the deployment's transport.
    #[must_use]
    pub fn new(
        pool: PgPool,
        tenant_id: impl Into<String>,
        transport: Option<Arc<dyn EmailTransport>>,
    ) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
            transport,
        }
    }

    /// Send up to `limit` due deliveries.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn deliver_due(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<DeliveryReport, EventError> {
        let mut report = DeliveryReport::default();
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let due = sqlx::query(
            "SELECT d.id, d.notification_id, d.attempts, n.title, n.kind, u.primary_email \
             FROM notification_deliveries d \
             JOIN notifications n ON n.tenant_id = d.tenant_id AND n.id = d.notification_id \
             JOIN users u ON u.id = n.user_id \
             WHERE d.tenant_id = $1 AND d.channel = 'email' AND d.status = 'pending' \
               AND d.next_attempt_at <= GREATEST($2, now()) \
             ORDER BY d.next_attempt_at, d.id LIMIT $3 FOR UPDATE OF d SKIP LOCKED",
        )
        .bind(&self.tenant_id)
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        for row in due {
            let id: String = row.try_get("id")?;
            let notification_id: String = row.try_get("notification_id")?;
            let attempts: i32 = row.try_get::<i32, _>("attempts")? + 1;
            let Some(transport) = &self.transport else {
                sqlx::query(
                    "UPDATE notification_deliveries SET status = 'unavailable', attempts = $3, \
                            last_error = 'no mail transport is configured (QUANSIO_EMAIL_ENDPOINT)' \
                     WHERE tenant_id = $1 AND id = $2",
                )
                .bind(&self.tenant_id)
                .bind(&id)
                .bind(attempts)
                .execute(&mut *tx)
                .await?;
                report.unavailable += 1;
                continue;
            };
            let title: Option<String> = row.try_get("title")?;
            let kind: String = row.try_get("kind")?;
            let message = EmailMessage {
                to: row.try_get("primary_email")?,
                subject: title.clone().unwrap_or_else(|| format!("Quansio: {kind}")),
                text: format!(
                    "{}\n\nOpen Quansio to see it.",
                    title.unwrap_or_else(|| format!("You have a new {kind} notification."))
                ),
            };
            match transport.send(&message).await {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE notification_deliveries SET status = 'delivered', attempts = $3, \
                                delivered_at = now(), last_error = NULL WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&self.tenant_id)
                    .bind(&id)
                    .bind(attempts)
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query(
                        "UPDATE notifications SET channels_delivered = channels_delivered || '[\"email\"]'::jsonb \
                         WHERE tenant_id = $1 AND id = $2 AND NOT channels_delivered ? 'email'",
                    )
                    .bind(&self.tenant_id)
                    .bind(&notification_id)
                    .execute(&mut *tx)
                    .await?;
                    report.delivered += 1;
                }
                Err(error) => {
                    let last = attempts >= MAX_EMAIL_ATTEMPTS;
                    sqlx::query(
                        "UPDATE notification_deliveries SET status = $3, attempts = $4, last_error = $5, \
                                next_attempt_at = $6 WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&self.tenant_id)
                    .bind(&id)
                    .bind(if last { "failed" } else { "pending" })
                    .bind(attempts)
                    .bind(error.chars().take(500).collect::<String>())
                    .bind(now + email_backoff(attempts))
                    .execute(&mut *tx)
                    .await?;
                    if last {
                        report.failed += 1;
                    } else {
                        report.retried += 1;
                    }
                }
            }
        }
        tx.commit().await?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_backoff_doubles_from_thirty_seconds() {
        assert_eq!(email_backoff(1), Duration::seconds(30));
        assert_eq!(email_backoff(3), Duration::seconds(120));
        assert_eq!(email_backoff(12), Duration::seconds(3600));
    }
}
