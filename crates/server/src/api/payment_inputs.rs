//! `SupplyPaymentInput` (ADP-009, DOMAIN.md §13.9 C, §14): the person's out-of-band supply of one checkout's payment values.
//!
//! The command is a person's. A service principal, a thread, a model and a tool cannot reach it: the handler refuses a
//! principal before it reads a value, and no tool in the registry carries the name. The values are sealed by
//! `quansio_machine::secrets::payment::PaymentInputs` (provider `payment_input`, usable once, overwritten when taken or when
//! their time runs out); this handler holds no SQL and answers with field names and expiry times only, never a value, so the
//! stored command outcome and the audit entry cannot hold one either.

use quansio_machine::secrets::payment::{
    PaymentInputConfig, PaymentInputError, PaymentInputs, Supply,
};
use serde_json::{json, Value};

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::audit::{append_in, AuditDraft};
use crate::policy::rbac::ActionFamily;

fn refuse(code: ApiErrorCode, message: impl Into<String>, correlation: &str) -> ApiError {
    ApiError::new(code, message.into(), correlation)
}

fn failure(error: PaymentInputError, correlation: &str) -> ApiError {
    match error {
        PaymentInputError::Disabled => refuse(
            ApiErrorCode::PolicyDenied,
            "payment inputs are switched off for this deployment",
            correlation,
        ),
        PaymentInputError::Invalid(detail) => {
            refuse(ApiErrorCode::ValidationSchema, detail, correlation)
        }
        PaymentInputError::Unavailable { field } => refuse(
            ApiErrorCode::NotFound,
            format!("no payment value is waiting for {field}"),
            correlation,
        ),
        PaymentInputError::Secret(_) | PaymentInputError::Database(_) => {
            eprintln!("quansio-server: payment input failed ({correlation})");
            refuse(
                ApiErrorCode::Internal,
                "the payment values could not be sealed; nothing was kept",
                correlation,
            )
        }
    }
}

/// The params with every value removed: what the command log's digest covers, so the digest of a short code is not a way
/// to recover it.
#[must_use]
pub fn redacted_params(params: &Value) -> Value {
    let mut redacted = params.clone();
    if let Some(fields) = redacted.get_mut("fields").and_then(Value::as_object_mut) {
        for value in fields.values_mut() {
            *value = Value::Null;
        }
    }
    redacted
}

/// `SupplyPaymentInput {workspace_id, origin, fields: {<field>: <value>}, ttl_seconds?}`.
pub async fn supply_payment_input(
    state: &ApiState,
    scope: &TenantScope,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    if scope.principal.is_some() {
        return Err(refuse(
            ApiErrorCode::ScopeForbidden,
            "payment values are a person's to supply: a service principal cannot",
            correlation,
        ));
    }
    let Some(user_id) = user_id else {
        return Err(refuse(
            ApiErrorCode::AuthRequired,
            "supplying payment values needs a signed-in person",
            correlation,
        ));
    };
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                refuse(
                    ApiErrorCode::ValidationSchema,
                    format!("{key} is required"),
                    correlation,
                )
            })
    };
    let workspace_id = text("workspace_id")?;
    let origin = text("origin")?;
    let Some(fields) = params.get("fields").and_then(Value::as_object) else {
        return Err(refuse(
            ApiErrorCode::ValidationSchema,
            "fields is required: an object of payment field to value",
            correlation,
        ));
    };
    let mut values: Vec<(&str, &[u8])> = Vec::with_capacity(fields.len());
    for (field, value) in fields {
        let Some(value) = value.as_str() else {
            return Err(refuse(
                ApiErrorCode::ValidationSchema,
                "every payment value is text",
                correlation,
            ));
        };
        values.push((field.as_str(), value.as_bytes()));
    }
    let ttl_seconds = match params.get("ttl_seconds") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_i64().ok_or_else(|| {
            refuse(
                ApiErrorCode::ValidationSchema,
                "ttl_seconds is a whole number of seconds",
                correlation,
            )
        })?),
    };
    // A person who can author work in the workspace can run its browser, and so can supply what its checkout needs.
    Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let Some(keys) = state.key_provider() else {
        return Err(refuse(
            ApiErrorCode::AdapterUnavailable,
            "this deployment has no key provider to seal payment values",
            correlation,
        ));
    };

    let at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut tx = state.tenant_tx(&scope.tenant_id, correlation).await?;
    let supplied = PaymentInputs::supply(
        &mut tx,
        keys.as_ref(),
        &PaymentInputConfig::builtin(),
        &Supply {
            tenant_id: &scope.tenant_id,
            workspace_id,
            origin,
            fields: values,
            ttl_seconds,
            at: &at,
        },
    )
    .await
    .map_err(|error| failure(error, correlation))?;
    let names: Vec<&str> = supplied.iter().map(|value| value.field.as_str()).collect();
    append_in(
        &mut tx,
        &scope.tenant_id,
        AuditDraft {
            workspace_id: Some(workspace_id.to_string()),
            actor: json!({ "kind": "user", "id": user_id }),
            action: "SupplyPaymentInput".to_string(),
            target_ref: Some(format!("workspace:{workspace_id}")),
            decision: Some("applied".to_string()),
            reason: Some(format!(
                "{} for {origin}; each usable once and overwritten when used or at its time",
                names.join(", ")
            )),
            correlation_id: Some(correlation.to_string()),
        },
    )
    .await
    .map_err(|error| refuse(ApiErrorCode::Internal, error.to_string(), correlation))?;
    tx.commit().await.map_err(|_| {
        refuse(
            ApiErrorCode::Internal,
            "the payment values could not be sealed; nothing was kept",
            correlation,
        )
    })?;
    Ok(json!({
        "workspace_id": workspace_id,
        "origin": origin,
        "supplied": supplied
            .iter()
            .map(|value| json!({ "field": value.field, "expires_at": value.expires_at }))
            .collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_keeps_the_field_names_and_drops_every_value() {
        let redacted = redacted_params(&json!({
            "workspace_id": "ws_x",
            "origin": "https://shop.example",
            "fields": { "card_number": "4242424242424242", "card_cvc": "6803" },
        }));
        assert_eq!(redacted["fields"]["card_number"], Value::Null);
        assert_eq!(redacted["fields"]["card_cvc"], Value::Null);
        assert!(!redacted.to_string().contains("4242"));
        assert_eq!(redacted["origin"], "https://shop.example");
    }
}
