//! Inbound hooks: filters, endpoints and the delivery ledger (ADP-007, DOMAIN.md §13.8 A).
//!
//! A routine whose trigger is `{kind: webhook}` fires when a signed HTTP request reaches its endpoint. This module owns what
//! the routine owner keeps about that: the endpoint's name, the typed filters that decide whether a delivery passes, and
//! `routine_hook_deliveries`, the ledger that makes a replayed delivery fire nothing. Authenticating the request is
//! `notify::webhooks::inbound` (it holds the signature scheme and reaches the secret broker); the payload only ever reaches a
//! run through `script::render_instruction` as an untrusted block.

use chrono::{DateTime, Duration, Utc};
use quansio_core::{EntropySource, UlidGenerator};
use quansio_events::{EventError, EventStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row};

use super::ledger::{claim_in, Fire, FireSource};

/// The prefix of an inbound endpoint's name.
pub const ENDPOINT_PREFIX: &str = "hk_";
/// The most groups a filter may have.
pub const MAX_FILTER_GROUPS: usize = 8;
/// The most conditions a group may have.
pub const MAX_GROUP_CONDITIONS: usize = 8;
/// The longest a condition's path may be.
pub const MAX_PATH_BYTES: usize = 128;
/// The most segments a condition's path may have.
pub const MAX_PATH_SEGMENTS: usize = 8;
/// The most values an `in` condition may name.
pub const MAX_IN_VALUES: usize = 32;
/// The longest a string value in a condition may be.
pub const MAX_VALUE_BYTES: usize = 256;

/// A comparison a condition makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOp {
    /// The field equals the value (string, number, boolean or null).
    Eq,
    /// The field is present and is not equal to the value.
    Ne,
    /// The field equals one of the values.
    In,
    /// The field's presence is the boolean value.
    Exists,
    /// A string field starts with the value.
    Prefix,
    /// A string field contains the value, or an array field has it as an element.
    Contains,
    /// A number field is greater than the value.
    Gt,
    /// A number field is at least the value.
    Gte,
    /// A number field is less than the value.
    Lt,
    /// A number field is at most the value.
    Lte,
}

/// One typed condition over the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookCondition {
    /// An RFC 6901 JSON Pointer into the payload.
    pub path: String,
    /// The comparison.
    pub op: HookOp,
    /// What the field is compared with.
    #[serde(default)]
    pub value: Value,
}

/// What evaluating a trigger's filters decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterOutcome {
    /// Whether the delivery passes.
    pub matched: bool,
    /// The index of the first group that held, when there are filters and one did.
    pub group: Option<usize>,
}

/// Refuse filters that are not within the bounds of DOMAIN.md §13.8 A.
///
/// # Errors
/// A sentence naming the first thing wrong.
pub fn validate_filters(filters: &[Vec<HookCondition>]) -> Result<(), String> {
    if filters.len() > MAX_FILTER_GROUPS {
        return Err(format!("filters has at most {MAX_FILTER_GROUPS} groups"));
    }
    for (group_index, group) in filters.iter().enumerate() {
        if group.is_empty() {
            return Err(format!("filter group {group_index} has no conditions"));
        }
        if group.len() > MAX_GROUP_CONDITIONS {
            return Err(format!(
                "filter group {group_index} has more than {MAX_GROUP_CONDITIONS} conditions"
            ));
        }
        for condition in group {
            let at = format!("filter group {group_index}, path {:?}", condition.path);
            if !condition.path.starts_with('/')
                || condition.path.len() > MAX_PATH_BYTES
                || condition.path.split('/').count() > MAX_PATH_SEGMENTS + 1
            {
                return Err(format!(
                    "{at}: a path is a JSON Pointer of at most {MAX_PATH_SEGMENTS} segments and {MAX_PATH_BYTES} bytes"
                ));
            }
            let scalar = |value: &Value| match value {
                Value::String(text) => text.len() <= MAX_VALUE_BYTES,
                Value::Number(_) | Value::Bool(_) | Value::Null => true,
                _ => false,
            };
            let valid = match condition.op {
                HookOp::Eq | HookOp::Ne => scalar(&condition.value),
                HookOp::In => condition.value.as_array().is_some_and(|values| {
                    !values.is_empty() && values.len() <= MAX_IN_VALUES && values.iter().all(scalar)
                }),
                HookOp::Exists => condition.value.is_boolean(),
                HookOp::Prefix | HookOp::Contains => condition
                    .value
                    .as_str()
                    .is_some_and(|text| !text.is_empty() && text.len() <= MAX_VALUE_BYTES),
                HookOp::Gt | HookOp::Gte | HookOp::Lt | HookOp::Lte => condition.value.is_number(),
            };
            if !valid {
                return Err(format!("{at}: the value does not fit {:?}", condition.op));
            }
        }
    }
    Ok(())
}

/// Whether a payload satisfies a trigger's filters: any group holds (OR), a group holds when every condition does (AND).
/// No filters means every payload passes. Values are typed and never coerced, and an absent field fails every operator
/// except `exists: false`, so a filter fails closed.
#[must_use]
pub fn evaluate_filters(filters: &[Vec<HookCondition>], payload: &Value) -> FilterOutcome {
    if filters.is_empty() {
        return FilterOutcome {
            matched: true,
            group: None,
        };
    }
    for (index, group) in filters.iter().enumerate() {
        if group.iter().all(|condition| holds(condition, payload)) {
            return FilterOutcome {
                matched: true,
                group: Some(index),
            };
        }
    }
    FilterOutcome {
        matched: false,
        group: None,
    }
}

fn scalar_equal(field: &Value, value: &Value) -> bool {
    match (field, value) {
        (Value::Number(a), Value::Number(b)) => a.as_f64().is_some() && a.as_f64() == b.as_f64(),
        (Value::String(_) | Value::Bool(_) | Value::Null, _) => field == value,
        _ => false,
    }
}

fn holds(condition: &HookCondition, payload: &Value) -> bool {
    let field = payload.pointer(&condition.path);
    let Some(field) = field else {
        return condition.op == HookOp::Exists && condition.value == Value::Bool(false);
    };
    let number = |value: &Value| value.as_f64();
    match condition.op {
        HookOp::Eq => scalar_equal(field, &condition.value),
        HookOp::Ne => !scalar_equal(field, &condition.value),
        HookOp::In => condition
            .value
            .as_array()
            .is_some_and(|values| values.iter().any(|value| scalar_equal(field, value))),
        HookOp::Exists => condition.value == Value::Bool(true),
        HookOp::Prefix => match (field.as_str(), condition.value.as_str()) {
            (Some(text), Some(prefix)) => text.starts_with(prefix),
            _ => false,
        },
        HookOp::Contains => match (field, condition.value.as_str()) {
            (Value::String(text), Some(needle)) => text.contains(needle),
            (Value::Array(items), Some(needle)) => {
                items.iter().any(|item| item.as_str() == Some(needle))
            }
            _ => false,
        },
        HookOp::Gt | HookOp::Gte | HookOp::Lt | HookOp::Lte => {
            let (Some(a), Some(b)) = (
                field.as_f64().filter(|_| field.is_number()),
                number(&condition.value),
            ) else {
                return false;
            };
            match condition.op {
                HookOp::Gt => a > b,
                HookOp::Gte => a >= b,
                HookOp::Lt => a < b,
                _ => a <= b,
            }
        }
    }
}

/// A new endpoint name: `hk_` and 43 characters of base64url from 32 random bytes.
#[must_use]
pub fn new_endpoint_id(entropy: &dyn EntropySource) -> String {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    entropy.fill(&mut bytes);
    format!(
        "{ENDPOINT_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// Whether a string has the shape of an endpoint name, so a lookup is never made with anything else.
#[must_use]
pub fn is_endpoint_id(candidate: &str) -> bool {
    candidate.len() == ENDPOINT_PREFIX.len() + 43
        && candidate.starts_with(ENDPOINT_PREFIX)
        && candidate[ENDPOINT_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The path an endpoint is served at.
#[must_use]
pub fn endpoint_path(endpoint_id: &str) -> String {
    format!("/v1/hooks/{endpoint_id}")
}

/// Whether a sender's delivery id is within the contract: 1 to 128 characters of `[A-Za-z0-9._:-]`.
#[must_use]
pub fn valid_delivery_id(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() <= 128
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// The digest of a signature header's value: what makes two byte-identical signed requests the same delivery.
#[must_use]
pub fn signature_digest(signature_header: &str) -> String {
    hex::encode(Sha256::digest(signature_header.trim().as_bytes()))
}

/// What accepting an authenticated delivery came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Accepted {
    /// A fire was claimed.
    Fired(Box<Fire>),
    /// The filters did not pass; the delivery is recorded and nothing fires.
    Filtered,
    /// The delivery was seen before (by its id or by its signature); nothing fires.
    Duplicate,
}

/// The delivery ledger of one tenant.
#[derive(Debug, Clone)]
pub struct HookLedger {
    events: EventStore,
    tenant_id: String,
}

/// What one delivery looks like to the ledger.
#[derive(Debug, Clone)]
pub struct IncomingDelivery<'a> {
    /// The routine.
    pub routine_id: &'a str,
    /// Its workspace.
    pub workspace_id: &'a str,
    /// The sender's delivery id, or the signature digest when it sent none.
    pub delivery_key: &'a str,
    /// [`signature_digest`] of the request.
    pub signature_digest: &'a str,
    /// The accepted JSON body.
    pub payload: &'a Value,
    /// What the trigger's filters decided.
    pub filter: FilterOutcome,
    /// How long identities are remembered.
    pub retention: Duration,
}

impl HookLedger {
    /// A ledger over the pool for one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// Record an authenticated delivery and, when it passes its filters, claim its fire — in one transaction, so a
    /// delivery is either remembered with its fire or not at all. A delivery whose id or signature is already
    /// remembered fires nothing.
    ///
    /// # Errors
    /// Returns a database or event error; nothing is recorded.
    pub async fn accept(
        &self,
        delivery: IncomingDelivery<'_>,
        now: DateTime<Utc>,
    ) -> Result<Accepted, EventError> {
        let tenant = self.tenant_id.clone();
        let routine_id = delivery.routine_id.to_string();
        let workspace_id = delivery.workspace_id.to_string();
        let key = delivery.delivery_key.to_string();
        let digest = delivery.signature_digest.to_string();
        let payload = delivery.payload.clone();
        let cutoff = now - delivery.retention;
        let row_id = format!("rhd_{}", UlidGenerator::new().generate());
        if !delivery.filter.matched {
            let mut tx = self.events.begin_tenant_transaction(&tenant).await?;
            sqlx::query(
                "DELETE FROM routine_hook_deliveries WHERE tenant_id = $1 AND routine_id = $2 AND received_at < $3",
            )
            .bind(&tenant)
            .bind(&routine_id)
            .bind(cutoff)
            .execute(&mut *tx)
            .await?;
            let inserted = sqlx::query(
                "INSERT INTO routine_hook_deliveries (id, tenant_id, workspace_id, routine_id, delivery_key, \
                 signature_digest, outcome, received_at) VALUES ($1, $2, $3, $4, $5, $6, 'filtered', $7) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(&row_id)
            .bind(&tenant)
            .bind(&workspace_id)
            .bind(&routine_id)
            .bind(&key)
            .bind(&digest)
            .bind(now)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            tx.commit().await?;
            return Ok(if inserted == 1 {
                Accepted::Filtered
            } else {
                Accepted::Duplicate
            });
        }
        let fire_key = format!("routine:{routine_id}:hook:{key}");
        let result = self
            .events
            .commit_mutation_tx(&self.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    sqlx::query(
                        "DELETE FROM routine_hook_deliveries WHERE tenant_id = $1 AND routine_id = $2 AND received_at < $3",
                    )
                    .bind(&tenant)
                    .bind(&routine_id)
                    .bind(cutoff)
                    .execute(&mut **tx)
                    .await?;
                    let inserted = sqlx::query(
                        "INSERT INTO routine_hook_deliveries (id, tenant_id, workspace_id, routine_id, delivery_key, \
                         signature_digest, outcome, received_at) VALUES ($1, $2, $3, $4, $5, $6, 'accepted', $7) \
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(&row_id)
                    .bind(&tenant)
                    .bind(&workspace_id)
                    .bind(&routine_id)
                    .bind(&key)
                    .bind(&digest)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?
                    .rows_affected();
                    if inserted == 0 {
                        // Seen before: nothing is staged, so the transaction is abandoned and there is no fire.
                        return Ok(None);
                    }
                    let fire = claim_in(
                        tx,
                        batch,
                        &tenant,
                        &workspace_id,
                        &routine_id,
                        &fire_key,
                        FireSource::Hook,
                        None,
                        Some(&key),
                        Some(&payload),
                    )
                    .await?;
                    if let Some(fire) = &fire {
                        sqlx::query(
                            "UPDATE routine_hook_deliveries SET fire_id = $4 \
                             WHERE tenant_id = $1 AND routine_id = $2 AND delivery_key = $3",
                        )
                        .bind(&tenant)
                        .bind(&routine_id)
                        .bind(&key)
                        .bind(&fire.id)
                        .execute(&mut **tx)
                        .await?;
                    }
                    Ok(fire)
                })
            })
            .await;
        match result {
            Ok(Some(fire)) => Ok(Accepted::Fired(Box::new(fire))),
            Ok(None) | Err(EventError::NoEventStaged) => Ok(Accepted::Duplicate),
            Err(error) => Err(error),
        }
    }

    /// What the ledger holds for a routine over the last day, by outcome, for the projection.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn summary(
        &self,
        routine_id: &str,
        since: DateTime<Utc>,
    ) -> Result<(u64, u64), EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(
            "SELECT outcome, count(*) AS n FROM routine_hook_deliveries \
             WHERE tenant_id = $1 AND routine_id = $2 AND received_at >= $3 GROUP BY outcome",
        )
        .bind(&self.tenant_id)
        .bind(routine_id)
        .bind(since)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut accepted = 0u64;
        let mut filtered = 0u64;
        for row in rows {
            let outcome: String = row.try_get("outcome")?;
            let n: i64 = row.try_get("n")?;
            let n = u64::try_from(n).unwrap_or(0);
            match outcome.as_str() {
                "accepted" => accepted = n,
                _ => filtered = n,
            }
        }
        Ok((accepted, filtered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn condition(path: &str, op: HookOp, value: Value) -> HookCondition {
        HookCondition {
            path: path.to_string(),
            op,
            value,
        }
    }

    #[test]
    fn any_group_may_hold_and_every_condition_of_a_group_must() {
        let payload = json!({
            "action": "opened",
            "pull_request": { "draft": false, "base": "main", "additions": 40, "labels": ["bug", "ui"], "title": "Fix the thing" },
            "sender": null
        });
        let filters = vec![
            vec![
                condition("/action", HookOp::Eq, json!("closed")),
                condition("/pull_request/base", HookOp::Eq, json!("main")),
            ],
            vec![
                condition("/action", HookOp::In, json!(["opened", "reopened"])),
                condition("/pull_request/draft", HookOp::Eq, json!(false)),
                condition("/pull_request/additions", HookOp::Gte, json!(40)),
                condition("/pull_request/labels", HookOp::Contains, json!("bug")),
                condition("/pull_request/title", HookOp::Prefix, json!("Fix")),
            ],
        ];
        let outcome = evaluate_filters(&filters, &payload);
        assert!(outcome.matched);
        assert_eq!(
            outcome.group,
            Some(1),
            "the first group fails on `action`, the second holds"
        );

        let none_hold = vec![vec![condition("/action", HookOp::Eq, json!("closed"))]];
        assert!(!evaluate_filters(&none_hold, &payload).matched);
        assert!(
            evaluate_filters(&[], &payload).matched,
            "no filters: every delivery passes"
        );
    }

    #[test]
    fn values_are_typed_and_an_absent_field_fails_closed() {
        let payload = json!({ "count": 3, "name": "3", "flag": true, "gone": null });
        let check = |path: &str, op: HookOp, value: Value| {
            evaluate_filters(&[vec![condition(path, op, value)]], &payload).matched
        };
        assert!(check("/count", HookOp::Eq, json!(3)));
        assert!(
            check("/count", HookOp::Eq, json!(3.0)),
            "numbers compare as numbers"
        );
        assert!(
            !check("/count", HookOp::Eq, json!("3")),
            "a number is not its text"
        );
        assert!(
            !check("/name", HookOp::Eq, json!(3)),
            "a string is not a number"
        );
        assert!(
            !check("/name", HookOp::Gt, json!(1)),
            "an ordering needs a number field"
        );
        assert!(
            !check("/flag", HookOp::Eq, json!(1)),
            "a boolean is not a number"
        );
        assert!(
            check("/gone", HookOp::Eq, json!(null)),
            "null is a value that is present"
        );
        assert!(check("/gone", HookOp::Exists, json!(true)));
        // Absent fields fail every operator except `exists: false`; `ne` does not hold of what is not there.
        assert!(!check("/missing", HookOp::Ne, json!("x")));
        assert!(!check("/missing", HookOp::Eq, json!(null)));
        assert!(!check("/missing", HookOp::In, json!(["a"])));
        assert!(check("/missing", HookOp::Exists, json!(false)));
        assert!(!check("/missing", HookOp::Exists, json!(true)));
        assert!(!check("/count", HookOp::Exists, json!(false)));
    }

    #[test]
    fn filters_are_bounded_and_typed_when_they_are_saved() {
        let ok = vec![vec![condition("/a/b", HookOp::Eq, json!("x"))]];
        assert!(validate_filters(&ok).is_ok());
        let too_many_groups = vec![ok[0].clone(); MAX_FILTER_GROUPS + 1];
        assert!(validate_filters(&too_many_groups).is_err());
        assert!(
            validate_filters(&[vec![]]).is_err(),
            "an empty group would always hold"
        );
        for bad in [
            condition("a/b", HookOp::Eq, json!("x")),
            condition("", HookOp::Exists, json!(true)),
            condition("/a/b/c/d/e/f/g/h/i", HookOp::Eq, json!("x")),
            condition("/a", HookOp::Eq, json!({"o": 1})),
            condition("/a", HookOp::In, json!([])),
            condition("/a", HookOp::In, json!("x")),
            condition("/a", HookOp::Exists, json!("yes")),
            condition("/a", HookOp::Gt, json!("1")),
            condition("/a", HookOp::Prefix, json!("")),
            condition("/a", HookOp::Contains, json!(5)),
        ] {
            assert!(validate_filters(&[vec![bad.clone()]]).is_err(), "{bad:?}");
        }
        // The wire shape refuses a condition with a field it does not know.
        assert!(serde_json::from_value::<HookCondition>(
            json!({"path": "/a", "op": "eq", "value": 1, "extra": true})
        )
        .is_err());
    }

    #[test]
    fn endpoints_are_unguessable_and_a_lookup_only_takes_their_shape() {
        let entropy = quansio_core::OsEntropy;
        let a = new_endpoint_id(&entropy);
        let b = new_endpoint_id(&entropy);
        assert_ne!(a, b);
        assert!(is_endpoint_id(&a), "{a}");
        assert_eq!(a.len(), 3 + 43);
        for bad in [
            "",
            "hk_",
            "hk_short",
            "rtn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE",
            &format!("hk_{}", "!".repeat(43)),
        ] {
            assert!(!is_endpoint_id(bad), "{bad:?}");
        }
        assert_eq!(endpoint_path(&a), format!("/v1/hooks/{a}"));
        assert!(valid_delivery_id("evt_1.A:b-c"));
        for bad in ["", "has space", "slash/", &"x".repeat(129)] {
            assert!(!valid_delivery_id(bad), "{bad:?}");
        }
        assert_eq!(
            signature_digest("t=1,v1=ab"),
            signature_digest("  t=1,v1=ab ")
        );
        assert_ne!(signature_digest("t=1,v1=ab"), signature_digest("t=2,v1=ab"));
    }
}
