//! `ImportBrowserSession` (ADP-009, DOMAIN.md §13.9 B, §14): a person stages cookies from another browser for the agent's browser.
//!
//! **Staging is not importing.** The command seals the bundle (`BrowserImports::stage`, provider `browser_import`) and answers with
//! its id, digest, count and domains; nothing reaches a browser until the approved `browser.import_session` call (effect class
//! `browser.session.import`, tier 4, always asked) takes it. The command is a person's: a service principal is refused before a
//! value is read, the caller must be able to cause a tier 4 effect in the workspace, and no Tool, thread, model output or routine
//! can carry it. The handler holds no SQL; the answer and the audit entry hold counts and domains, never a value.

use quansio_machine::secrets::imports::{
    BrowserImports, ImportConfig, ImportCookie, ImportError, StageRequest,
};
use serde_json::{json, Value};

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::audit::{append_in, AuditDraft};
use crate::policy::rbac::ActionFamily;

fn refuse(code: ApiErrorCode, message: impl Into<String>, correlation: &str) -> ApiError {
    ApiError::new(code, message.into(), correlation)
}

fn failure(error: ImportError, correlation: &str) -> ApiError {
    match error {
        ImportError::Disabled => refuse(
            ApiErrorCode::PolicyDenied,
            "browser-data imports are switched off for this deployment",
            correlation,
        ),
        ImportError::Invalid(detail) => refuse(ApiErrorCode::ValidationSchema, detail, correlation),
        ImportError::DigestMismatch => refuse(
            ApiErrorCode::ValidationSchema,
            "the digest is not the digest of the cookies sent; nothing was kept",
            correlation,
        ),
        ImportError::Unavailable => refuse(
            ApiErrorCode::NotFound,
            "no staged bundle is waiting under this id",
            correlation,
        ),
        ImportError::Secret(_) | ImportError::Database(_) => {
            eprintln!("quansio-server: browser import failed ({correlation})");
            refuse(
                ApiErrorCode::Internal,
                "the cookies could not be sealed; nothing was kept",
                correlation,
            )
        }
    }
}

/// The params with every cookie value removed: what the command log's digest covers.
#[must_use]
pub fn redacted_params(params: &Value) -> Value {
    let mut redacted = params.clone();
    if let Some(cookies) = redacted.get_mut("cookies").and_then(Value::as_array_mut) {
        for cookie in cookies {
            if let Some(value) = cookie.get_mut("value") {
                *value = Value::Null;
            }
        }
    }
    redacted
}

/// `ImportBrowserSession {workspace_id, cookies: [ImportCookie], digest}`.
pub async fn import_browser_session(
    state: &ApiState,
    scope: &TenantScope,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    if scope.principal.is_some() {
        return Err(refuse(
            ApiErrorCode::ScopeForbidden,
            "importing another browser's cookies is a person's act: a service principal cannot",
            correlation,
        ));
    }
    let Some(user_id) = user_id else {
        return Err(refuse(
            ApiErrorCode::AuthRequired,
            "importing browser data needs a signed-in person",
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
    let digest = text("digest")?;
    let Some(raw) = params.get("cookies").and_then(Value::as_array) else {
        return Err(refuse(
            ApiErrorCode::ValidationSchema,
            "cookies is required: a list of cookies",
            correlation,
        ));
    };
    let config = ImportConfig::builtin();
    // Bound the list before it is parsed: a request cannot make the handler build an unbounded value.
    if raw.len() > config.max_cookies {
        return Err(refuse(
            ApiErrorCode::ValidationSchema,
            format!("a bundle carries at most {} cookies", config.max_cookies),
            correlation,
        ));
    }
    let mut cookies: Vec<ImportCookie> = Vec::with_capacity(raw.len());
    for item in raw {
        let cookie: ImportCookie = serde_json::from_value(item.clone()).map_err(|_| {
            refuse(
                ApiErrorCode::ValidationSchema,
                "a cookie is {name, value, domain, path, secure, http_only, same_site, expires}",
                correlation,
            )
        })?;
        cookies.push(cookie);
    }
    // Staging prepares a tier 4 effect, so it needs the role that may cause one.
    Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::ExecuteEffect { tier: 4 },
        correlation,
    )
    .await?;
    let Some(keys) = state.key_provider() else {
        return Err(refuse(
            ApiErrorCode::AdapterUnavailable,
            "this deployment has no key provider to seal browser data",
            correlation,
        ));
    };

    let at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut tx = state.tenant_tx(&scope.tenant_id, correlation).await?;
    let staged = BrowserImports::stage(
        &mut tx,
        keys.as_ref(),
        &config,
        &StageRequest {
            tenant_id: &scope.tenant_id,
            workspace_id,
            cookies: &cookies,
            claimed_digest: digest,
            at: &at,
        },
    )
    .await
    .map_err(|error| failure(error, correlation))?;
    let listed = config.max_domains_listed.min(staged.domains.len());
    append_in(
        &mut tx,
        &scope.tenant_id,
        AuditDraft {
            workspace_id: Some(workspace_id.to_string()),
            actor: json!({ "kind": "user", "id": user_id }),
            action: "ImportBrowserSession".to_string(),
            target_ref: Some(format!("browser_import:{}", staged.import_id)),
            decision: Some("applied".to_string()),
            reason: Some(format!(
                "{} cookies for {} domains staged; nothing reaches a browser until browser.import_session is approved",
                staged.cookie_count,
                staged.domains.len()
            )),
            correlation_id: Some(correlation.to_string()),
        },
    )
    .await
    .map_err(|error| refuse(ApiErrorCode::Internal, error.to_string(), correlation))?;
    tx.commit().await.map_err(|_| {
        refuse(
            ApiErrorCode::Internal,
            "the cookies could not be sealed; nothing was kept",
            correlation,
        )
    })?;
    Ok(json!({
        "workspace_id": workspace_id,
        "import_id": staged.import_id,
        "digest": staged.digest,
        "cookie_count": staged.cookie_count,
        "domains": staged.domains[..listed],
        "more_domains": staged.domains.len() - listed,
        "expires_at": staged.expires_at,
        "state": "staged",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_drops_every_cookie_value_and_keeps_the_rest() {
        let redacted = redacted_params(&json!({
            "workspace_id": "ws_x",
            "digest": "d",
            "cookies": [
                { "name": "session", "value": "s3cr3t-canary", "domain": "example.test", "path": "/" },
                { "name": "prefs", "value": "dark", "domain": "example.test", "path": "/" },
            ],
        }));
        assert!(!redacted.to_string().contains("s3cr3t-canary"));
        assert_eq!(redacted["cookies"][0]["value"], Value::Null);
        assert_eq!(redacted["cookies"][1]["name"], "prefs");
    }
}
