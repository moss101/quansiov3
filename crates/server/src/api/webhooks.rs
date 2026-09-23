//! Outbound webhooks over the public API (APP-015, DOMAIN.md §13.5, §14).
//!
//! Commands, admin-only (`ManageWorkspace` for a workspace subscription, `ManageTenant` for a
//! tenant-wide one) and audited by the owner: `CreateWebhookSubscription {url, workspace_id?,
//! event_types?}` (the result carries the signing secret once), `UpdateWebhookSubscription
//! {subscription_id, status?, event_types?, secret?: "rotate"}`, `RedeliverWebhook
//! {delivery_id}`, `DeleteWebhookSubscription {subscription_id}`. Projections:
//! `GET /v1/webhooks?workspace_id=` and `GET /v1/webhooks/{id}/deliveries?limit=`.
//!
//! Persistence and delivery are `notify::webhooks`'; this module is the HTTP path.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_events::Actor;
use serde::Deserialize;
use serde_json::{json, Value};

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::notify::webhooks::{
    NewSubscription, SubscriptionChange, WebhookError, WebhookStore, WebhookSubscription,
};
use crate::policy::rbac::ActionFamily;

/// The result field that carries a one-time value. The command log stores the result without
/// it, so a replayed command never returns a secret a second time.
pub const ONE_TIME_FIELD: &str = "secret";

fn webhook_error(error: WebhookError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        "POLICY_DENIED" => ApiErrorCode::PolicyDenied,
        "EGRESS_DENIED" => ApiErrorCode::EgressDenied,
        _ => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn required<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    text(params, key).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("{key} is required"),
            correlation,
        )
    })
}

fn event_types(params: &Value, correlation: &str) -> Result<Option<Vec<String>>, ApiError> {
    let Some(value) = params.get("event_types") else {
        return Ok(None);
    };
    value
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .map(Some)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "event_types is an array of event types (`run.succeeded`) or families (`run.*`)",
                correlation,
            )
        })
}

fn actor(scope: &TenantScope) -> Actor {
    scope
        .user_id
        .clone()
        .map_or_else(|| Actor::system("api"), Actor::user)
}

fn store(state: &ApiState, scope: &TenantScope) -> WebhookStore {
    WebhookStore::new(state.pool().clone(), scope.tenant_id.clone())
}

/// Webhook management is administration of the scope the subscription reaches.
async fn authorize(
    state: &ApiState,
    scope: &TenantScope,
    workspace_id: Option<&str>,
    correlation: &str,
) -> Result<(), ApiError> {
    let action = if workspace_id.is_some() {
        ActionFamily::ManageWorkspace
    } else {
        ActionFamily::ManageTenant
    };
    Authorization::check(state.pool(), scope, workspace_id, action, correlation).await?;
    Ok(())
}

/// Load a subscription and authorize a change to it.
async fn managed(
    state: &ApiState,
    scope: &TenantScope,
    subscription_id: &str,
    correlation: &str,
) -> Result<WebhookSubscription, ApiError> {
    let subscription = store(state, scope)
        .load(subscription_id)
        .await
        .map_err(|error| webhook_error(error, correlation))?;
    authorize(
        state,
        scope,
        subscription.workspace_id.as_deref(),
        correlation,
    )
    .await?;
    Ok(subscription)
}

/// `CreateWebhookSubscription {url, workspace_id?, event_types?}`.
pub async fn create_subscription(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let url = required(params, "url", correlation)?;
    let workspace_id = text(params, "workspace_id");
    authorize(state, scope, workspace_id, correlation).await?;
    let (subscription, secret) = store(state, scope)
        .create(
            &actor(scope),
            correlation,
            NewSubscription {
                workspace_id: workspace_id.map(str::to_string),
                url: url.to_string(),
                event_types: event_types(params, correlation)?.unwrap_or_default(),
                created_by: scope.user_id.clone(),
            },
            state.key_provider(),
        )
        .await
        .map_err(|error| webhook_error(error, correlation))?;
    Ok(json!({ "subscription": subscription, ONE_TIME_FIELD: secret }))
}

/// `UpdateWebhookSubscription {subscription_id, status?, event_types?, secret?: "rotate"}`.
pub async fn update_subscription(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let id = required(params, "subscription_id", correlation)?;
    managed(state, scope, id, correlation).await?;
    let rotate_secret = match params.get("secret") {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) if value == "rotate" => true,
        Some(_) => {
            return Err(ApiError::new(
                ApiErrorCode::ValidationSchema,
                "secret is \"rotate\" when given",
                correlation,
            ))
        }
    };
    let (subscription, secret) = store(state, scope)
        .update(
            &actor(scope),
            correlation,
            id,
            SubscriptionChange {
                status: text(params, "status").map(str::to_string),
                event_types: event_types(params, correlation)?,
                rotate_secret,
            },
            state.key_provider(),
        )
        .await
        .map_err(|error| webhook_error(error, correlation))?;
    let mut result = json!({ "subscription": subscription });
    if let Some(secret) = secret {
        result[ONE_TIME_FIELD] = Value::String(secret);
    }
    Ok(result)
}

/// `RedeliverWebhook {delivery_id}`.
pub async fn redeliver(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let delivery_id = required(params, "delivery_id", correlation)?;
    let subscription_id = store(state, scope)
        .subscription_of_delivery(delivery_id)
        .await
        .map_err(|error| webhook_error(error, correlation))?;
    managed(state, scope, &subscription_id, correlation).await?;
    let delivery = store(state, scope)
        .redeliver(&actor(scope), correlation, delivery_id)
        .await
        .map_err(|error| webhook_error(error, correlation))?;
    Ok(json!({ "delivery": delivery }))
}

/// `DeleteWebhookSubscription {subscription_id}`.
pub async fn delete_subscription(
    state: &ApiState,
    scope: &TenantScope,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let id = required(params, "subscription_id", correlation)?;
    managed(state, scope, id, correlation).await?;
    let subscription = store(state, scope)
        .delete(&actor(scope), correlation, id)
        .await
        .map_err(|error| webhook_error(error, correlation))?;
    Ok(json!({ "subscription": subscription, "deleted": true }))
}

/// The result as the command log keeps it: without a one-time value.
#[must_use]
pub fn stored_result(result: &Value) -> Value {
    match result {
        Value::Object(map) if map.contains_key(ONE_TIME_FIELD) => {
            let mut stored = map.clone();
            stored.insert(ONE_TIME_FIELD.to_string(), Value::Null);
            stored.insert("secret_returned_once".to_string(), Value::Bool(true));
            Value::Object(stored)
        }
        other => other.clone(),
    }
}

/// `?workspace_id=` for the list projection.
#[derive(Debug, Deserialize)]
pub struct WebhooksQuery {
    #[serde(default)]
    workspace_id: Option<String>,
}

/// `GET /v1/webhooks?workspace_id=`: subscriptions with their delivery summary. Without a
/// workspace, the tenant's (tenant administrators); with one, that workspace's and the
/// tenant-wide ones (the workspace's administrators).
pub async fn webhooks(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WebhooksQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    authorize(&state, &scope, query.workspace_id.as_deref(), "projection").await?;
    let items = store(&state, &scope)
        .list(query.workspace_id.as_deref())
        .await
        .map_err(|error| webhook_error(error, "projection"))?
        .into_iter()
        .map(|(subscription, counts)| {
            let mut item = serde_json::to_value(subscription).unwrap_or(Value::Null);
            item["deliveries"] = counts;
            item
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "workspace_id": query.workspace_id,
        "items": items,
        "cursor": Value::Null,
    })))
}

/// `?limit=` for the deliveries projection.
#[derive(Debug, Deserialize)]
pub struct DeliveriesQuery {
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /v1/webhooks/{id}/deliveries?limit=`: the ledger, newest event first.
pub async fn deliveries(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<DeliveriesQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let subscription = managed(&state, &scope, &id, "projection").await?;
    let items = store(&state, &scope)
        .deliveries(&subscription.id, query.limit.unwrap_or(100))
        .await
        .map_err(|error| webhook_error(error, "projection"))?;
    Ok(Json(json!({
        "subscription_id": subscription.id,
        "items": items,
        "cursor": Value::Null,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_log_never_keeps_a_one_time_secret() {
        let result = json!({ "subscription": { "id": "whk_x" }, "secret": "whsec_abc" });
        let stored = stored_result(&result);
        assert_eq!(stored["secret"], Value::Null);
        assert_eq!(stored["secret_returned_once"], true);
        assert!(!stored.to_string().contains("whsec_abc"));
        let plain = json!({ "subscription": { "id": "whk_x" } });
        assert_eq!(stored_result(&plain), plain);
    }
}
