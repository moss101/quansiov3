//! Outbound webhooks (APP-015, DOMAIN.md §13.5).
//!
//! Integrators receive the same canonical RuntimeEvents clients do, pushed:
//!
//! * [`store`] — subscriptions (the destination's egress grant, created by an admin, with a
//!   server-generated signing secret kept as a SecretHandle) and the delivery ledger.
//! * [`fanout`] — the event-log consumer that enqueues one delivery per (subscription,
//!   event) in the transaction that advances its cursor.
//! * [`deliverer`] — leases due deliveries and attempts each through the egress broker under
//!   the network policy as it is at the attempt, signs the redacted envelope, and records the
//!   outcome: delivered, retried after backoff, or dead-lettered.
//! * [`signature`] and [`redact`] — the `quansio-signature` scheme and payload redaction.
//!
//! Delivery is at-least-once under a stable `event_id`. Events of the `webhook` family are
//! never delivered, so a delivery cannot cause a delivery.

pub mod deliverer;
pub mod fanout;
pub mod redact;
pub mod signature;
pub mod store;

use chrono::{DateTime, Duration, Utc};
use quansio_events::{EventError, RuntimeEvent, EVENT_FAMILIES};
use quansio_machine::egress::{NetworkPolicy, NETWORK_EFFECT_CLASS};
use quansio_machine::secrets::SecretError;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};

pub use deliverer::{
    HostResolver, SystemResolver, WebhookDeliverer, WebhookDeliveryReport, WEBHOOK_LEASE_SECONDS,
};
pub use fanout::{WebhookFanout, FANOUT_CONSUMER, FANOUT_STREAM};
pub use redact::{is_secret_key, redact_payload, REDACTED_PROTECTED, REDACTED_SECRET};
pub use signature::{
    generate_secret, sign, verify, SignatureError, DELIVERY_ID_HEADER, EVENT_ID_HEADER,
    EVENT_TYPE_HEADER, SECRET_PREFIX, SIGNATURE_HEADER, SIGNATURE_SCHEME,
    SIGNATURE_TOLERANCE_SECONDS,
};
pub use store::{
    NewSubscription, SubscriptionChange, WebhookDelivery, WebhookStore, WebhookSubscription,
};

/// Repository path of this module's canonical owner.
pub const WEBHOOKS_OWNER: &str = "crates/server/src/notify/webhooks";

/// The event family webhooks never deliver.
pub const WEBHOOK_FAMILY: &str = "webhook";

/// A delivery that failed this many attempts is dead-lettered.
pub const MAX_WEBHOOK_ATTEMPTS: i32 = 8;

/// The SecretHandle provider name of a signing secret.
pub const SECRET_PROVIDER: &str = "webhook";

/// Subscription statuses (DOMAIN.md §13.5).
pub const SUBSCRIPTION_STATUSES: &[&str] = &["active", "paused", "failing"];

/// Why a webhook operation was refused.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The event store or database refused.
    #[error("webhook store: {0}")]
    Event(#[from] EventError),
    /// PostgreSQL rejected a statement.
    #[error("webhook database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The secret broker refused.
    #[error("webhook signing secret: {0}")]
    Secret(#[from] SecretError),
    /// The request is malformed.
    #[error("{0}")]
    Validation(String),
    /// No such subscription or delivery here.
    #[error("{0} was not found")]
    NotFound(String),
    /// The state does not allow it.
    #[error("{0}")]
    Conflict(String),
    /// The network policy does not permit the destination.
    #[error("{0}")]
    PolicyDenied(String),
    /// The egress broker refuses the destination.
    #[error("{0}")]
    EgressDenied(String),
    /// The deployment has no key provider to seal a signing secret.
    #[error(
        "this deployment has no secret key provider (QUANSIO_SECRETS_KEY_FILE), so a webhook \
         cannot be given a signing secret"
    )]
    SigningUnavailable,
}

impl WebhookError {
    /// DOMAIN.md §15 code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT_STATE",
            Self::PolicyDenied(_) => "POLICY_DENIED",
            Self::EgressDenied(_) => "EGRESS_DENIED",
            Self::Event(_) | Self::Database(_) | Self::Secret(_) | Self::SigningUnavailable => {
                "INTERNAL"
            }
        }
    }
}

/// Refuse an event-type filter that is not `family.name` or `family.*` over a deliverable
/// family.
///
/// # Errors
/// Returns [`WebhookError::Validation`] naming the entry.
pub fn validate_event_types(types: &[String]) -> Result<(), WebhookError> {
    if types.len() > 64 {
        return Err(WebhookError::Validation(
            "a subscription filters on at most 64 event types".to_string(),
        ));
    }
    for spec in types {
        let invalid = |why: &str| WebhookError::Validation(format!("event type {spec:?}: {why}"));
        let (family, name) = spec
            .split_once('.')
            .ok_or_else(|| invalid("is family.name or family.*"))?;
        if !EVENT_FAMILIES.contains(&family) {
            return Err(invalid("not an event family"));
        }
        if family == WEBHOOK_FAMILY {
            return Err(invalid("webhook events are never delivered"));
        }
        let name_ok = name == "*"
            || (!name.is_empty()
                && name.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                }));
        if !name_ok {
            return Err(invalid("the event name is snake_case or *"));
        }
    }
    Ok(())
}

/// Whether an event type passes a subscription's filter (empty selects every type).
#[must_use]
pub fn event_selected(filters: &[String], event_type: &str) -> bool {
    filters.is_empty()
        || filters.iter().any(|spec| match spec.strip_suffix(".*") {
            Some(family) => event_type
                .split_once('.')
                .is_some_and(|(event_family, _)| event_family == family),
            None => spec == event_type,
        })
}

/// Whether an event may be delivered to a webhook at all.
#[must_use]
pub fn deliverable(event: &RuntimeEvent) -> bool {
    event.event_type.family().as_str() != WEBHOOK_FAMILY
}

/// The wait before the next attempt after `attempts` failed ones: 30 s · 2^(attempts − 1),
/// capped at an hour.
#[must_use]
pub fn webhook_backoff(attempts: i32) -> Duration {
    let exponent = u32::try_from(attempts.saturating_sub(1).clamp(0, 16)).unwrap_or(0);
    let seconds = 30_i64.saturating_mul(1_i64 << exponent).min(3600);
    Duration::seconds(seconds)
}

/// Canonical `YYYY-MM-DDTHH:MM:SSZ`, the instant shape the machine brokers compare.
#[must_use]
pub fn canonical_instant(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The network policy in force for a webhook: the tenant policy's `network.egress.new_destination`
/// rule and, for a workspace, the workspace policy's. `None` when neither has one — egress is
/// then denied, as it is for an execution target without a rule (EXEC-008).
///
/// Several rules combine to the most restrictive: any `deny` denies, and only the address
/// classes every rule allows are allowed.
///
/// # Errors
/// [`WebhookError::PolicyDenied`] when a network rule cannot be understood (a policy whose
/// meaning is guessed is not a policy); a database error otherwise.
pub async fn network_policy_in(
    conn: &mut PgConnection,
    tenant_id: &str,
    workspace_id: Option<&str>,
) -> Result<Option<NetworkPolicy>, WebhookError> {
    let rows = sqlx::query(
        "SELECT id, version, rules FROM policies WHERE tenant_id = $1 \
           AND ((scope = 'tenant' AND workspace_id IS NULL) \
            OR (scope = 'workspace' AND workspace_id = $2)) \
         ORDER BY (scope = 'workspace'), id",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut combined: Option<(String, i32, bool, Vec<String>)> = None;
    for row in &rows {
        let id: String = row.try_get("id")?;
        let version: i32 = row.try_get("version")?;
        let rules: Value = row.try_get("rules")?;
        let Some(rule) = rules.as_array().and_then(|rules| {
            rules.iter().find(|rule| {
                rule.get("effect_class").and_then(Value::as_str) == Some(NETWORK_EFFECT_CLASS)
            })
        }) else {
            continue;
        };
        let policy = NetworkPolicy::from_rules(&id, version, &json!([rule])).map_err(|error| {
            WebhookError::PolicyDenied(format!("the network rule of {id} is not usable: {error}"))
        })?;
        let denies = policy.decision() == quansio_machine::egress::PolicyRuleDecision::Deny;
        let classes: Vec<String> = policy
            .allowed_classes()
            .iter()
            .map(|class| class.as_str().to_string())
            .collect();
        combined = Some(match combined {
            None => (id, version, denies, classes),
            Some((_, _, denied, allowed)) => (
                id,
                version,
                denied || denies,
                allowed
                    .into_iter()
                    .filter(|class| classes.contains(class))
                    .collect(),
            ),
        });
    }
    let Some((id, version, denies, classes)) = combined else {
        return Ok(None);
    };
    if denies {
        return Ok(Some(NetworkPolicy::denying(id, version)));
    }
    let rule = json!([{
        "effect_class": NETWORK_EFFECT_CLASS,
        "decision": "allow",
        "conditions": { "allowed_address_classes": classes },
    }]);
    NetworkPolicy::from_rules(&id, version, &rule)
        .map(Some)
        .map_err(|error| WebhookError::PolicyDenied(error.to_string()))
}

/// A projection-friendly rendering of an instant.
pub(crate) fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_family_wildcards_or_exact_types() {
        assert!(validate_event_types(&["run.*".into(), "thread.message_posted".into()]).is_ok());
        for bad in ["run", "nope.*", "webhook.*", "run.Succeeded", "run."] {
            assert!(validate_event_types(&[bad.to_string()]).is_err(), "{bad}");
        }
        assert!(event_selected(&[], "run.succeeded"));
        assert!(event_selected(&["run.*".into()], "run.succeeded"));
        assert!(!event_selected(&["run.*".into()], "runner.x"));
        assert!(event_selected(&["run.succeeded".into()], "run.succeeded"));
        assert!(!event_selected(&["run.succeeded".into()], "run.failed"));
    }

    #[test]
    fn backoff_doubles_from_thirty_seconds_to_an_hour() {
        assert_eq!(webhook_backoff(1), Duration::seconds(30));
        assert_eq!(webhook_backoff(2), Duration::seconds(60));
        assert_eq!(webhook_backoff(4), Duration::seconds(240));
        assert_eq!(webhook_backoff(8), Duration::seconds(3600));
        assert_eq!(webhook_backoff(40), Duration::seconds(3600));
    }
}
