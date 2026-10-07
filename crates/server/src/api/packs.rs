//! Pack installs through the API (ADP-008, DOMAIN.md 11.6, 14): `InstallPack`, `RollbackPack` and `GET /v1/packs`.
//!
//! Installing changes what the whole tenant may use, so both commands are a tenant administrator's (the dispatcher
//! checks `ManageTenant` first, as for `InstallSkill`). The handlers hold no SQL: the revisions, the activation and the
//! install reports belong to `control::packs`. A failed install is `PACK_INSTALL_FAILED` carrying the report's phase,
//! outcome and redacted diagnostics; the report itself is kept either way and is what the read returns.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_core::CommandId;
use quansio_events::Actor;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::packs::{
    GatewayPackCompiler, InstallError, InstallRequest, PackConfig, PackContent, PackError,
    PackInstaller, PackSource, PackStore,
};
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::RuntimeIdentity;

fn invalid(message: impl Into<String>, correlation: &str) -> ApiError {
    ApiError::new(ApiErrorCode::ValidationSchema, message, correlation)
}

fn store(state: &ApiState) -> PackStore {
    PackStore::new(
        state.pool().clone(),
        Arc::new(PackConfig::committed().clone()),
    )
}

fn installer(state: &ApiState) -> PackInstaller {
    let deadline = crate::intelligence::model_source::ModelCallSettings::load()
        .map_or(std::time::Duration::from_secs(60), |settings| {
            settings.deadline
        });
    let compiler = state.intelligence().cloned().map(|client| {
        Arc::new(GatewayPackCompiler::new(client, deadline))
            as Arc<dyn crate::control::packs::PackCompilePort>
    });
    PackInstaller::new(store(state), compiler)
}

fn actor(user_id: Option<&str>) -> Actor {
    user_id.map_or_else(|| Actor::system("api"), Actor::user)
}

fn pack_error(error: PackError, correlation: &str) -> ApiError {
    let code = ApiErrorCode::ALL
        .into_iter()
        .find(|known| known.as_str() == error.code())
        .unwrap_or(ApiErrorCode::Internal);
    if code == ApiErrorCode::Internal {
        eprintln!("quansio-server: pack store failed ({correlation}): {error}");
        return ApiError::new(
            code,
            "the pack could not be processed; try again",
            correlation,
        );
    }
    let refusal = ApiError::new(code, error.to_string(), correlation);
    match &error {
        PackError::Conflict {
            pack,
            expected,
            current,
        } => refusal.with_details(json!({
            "pack": pack,
            "expected_revision": expected,
            "current_revision": current,
        })),
        _ => refusal,
    }
}

/// An optional revision-or-null parameter: absent is "no expectation", `null` is "nothing is active".
fn expected_active(params: &Value, correlation: &str) -> Result<Option<Option<String>>, ApiError> {
    match params.get("expected_active_revision") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(revision)) if is_revision(revision) => Ok(Some(Some(revision.clone()))),
        Some(_) => Err(invalid(
            "expected_active_revision is null or sha256:<64 hex digits>",
            correlation,
        )),
    }
}

fn is_revision(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

fn source_of(params: &Value, correlation: &str) -> Result<PackSource, ApiError> {
    let source = params
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            invalid(
                "source is required: {kind: inline|builtin, ...}",
                correlation,
            )
        })?;
    let closed = |allowed: &[&str]| -> Result<(), ApiError> {
        match source.keys().find(|key| !allowed.contains(&key.as_str())) {
            Some(key) => Err(ApiError::new(
                ApiErrorCode::ValidationUnknownField,
                format!("source.{key} is not a field of this source"),
                correlation,
            )),
            None => Ok(()),
        }
    };
    match source.get("kind").and_then(Value::as_str) {
        Some("inline") => {
            closed(&["kind", "manifest", "files"])?;
            let manifest = source
                .get("manifest")
                .filter(|manifest| manifest.is_object())
                .ok_or_else(|| invalid("source.manifest must be an object", correlation))?
                .clone();
            let mut files = std::collections::BTreeMap::new();
            if let Some(entries) = source.get("files") {
                let entries = entries.as_object().ok_or_else(|| {
                    invalid(
                        "source.files must be an object of path to text",
                        correlation,
                    )
                })?;
                for (path, text) in entries {
                    let text = text.as_str().ok_or_else(|| {
                        invalid(format!("source.files.{path} must be text"), correlation)
                    })?;
                    files.insert(path.clone(), text.to_string());
                }
            }
            Ok(PackSource::Inline(PackContent { manifest, files }))
        }
        Some("builtin") => {
            closed(&["kind", "name"])?;
            let name = source
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("source.name is required", correlation))?;
            Ok(PackSource::Builtin(name.to_string()))
        }
        _ => Err(invalid("source.kind is inline or builtin", correlation)),
    }
}

fn failure_error(report: &crate::control::packs::InstallReport, correlation: &str) -> ApiError {
    let conflict = report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.rule == "pack.activate.conflict");
    let details = json!({
        "request_id": report.request_id,
        "pack": report.pack_name,
        "phase": report.phase.as_str(),
        "outcome": report.outcome.map(crate::control::packs::Outcome::as_str),
        "revision": report.revision,
        "diagnostics": report.diagnostics.iter().map(crate::control::packs::Diagnostic::to_json).collect::<Vec<_>>(),
        "diagnostics_omitted": report.diagnostics_omitted,
    });
    if conflict {
        return ApiError::new(
            ApiErrorCode::ConflictRevision,
            "the pack's active revision is not the one this install expected; the active revision is untouched",
            correlation,
        )
        .with_details(details);
    }
    ApiError::new(
        ApiErrorCode::PackInstallFailed,
        format!(
            "the pack could not be installed ({} failed); {}",
            report.phase.as_str(),
            match report.outcome {
                Some(crate::control::packs::Outcome::RetainedPrevious) =>
                    "the revision that was active still is",
                _ => "nothing was activated",
            }
        ),
        correlation,
    )
    .with_details(details)
}

/// `InstallPack {workspace_id, source, expected_revision?, expected_active_revision?}`.
///
/// # Errors
/// Malformed parameters, `PACK_INSTALL_FAILED` (the report says where) or `CONFLICT_REVISION`.
pub async fn install_pack(
    state: &ApiState,
    identity: &RuntimeIdentity,
    command_id: &CommandId,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let workspace_id = params
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            invalid(
                "workspace_id is required: the workspace the administrator acts from",
                correlation,
            )
        })?;
    let expected_revision = match params.get("expected_revision") {
        None | Some(Value::Null) => None,
        Some(Value::String(revision)) if is_revision(revision) => Some(revision.clone()),
        Some(_) => {
            return Err(invalid(
                "expected_revision is sha256:<64 hex digits>",
                correlation,
            ))
        }
    };
    let request = InstallRequest {
        request_id: command_id.as_canonical().to_string(),
        workspace_id: workspace_id.to_string(),
        source: source_of(params, correlation)?,
        expected_revision,
        expected_active: expected_active(params, correlation)?,
        actor: actor(user_id),
        correlation_id: identity.correlation_id,
    };
    match installer(state).install(&identity.tenant_id, request).await {
        Ok(installed) => {
            let report = &installed.report;
            Ok(json!({
                "request_id": report.request_id,
                "pack": report.pack_name,
                "pack_id": report.pack_id,
                "revision": report.revision,
                "previous_revision": report.previous_revision,
                "outcome": report.outcome.map(crate::control::packs::Outcome::as_str),
                "unchanged": installed.unchanged,
                "replayed": installed.replayed,
                "evicted": installed.evicted,
                "report": report.to_json(),
            }))
        }
        Err(InstallError::Failed(report)) => Err(failure_error(&report, correlation)),
        Err(InstallError::Store(error)) => Err(pack_error(error, correlation)),
    }
}

/// `RollbackPack {pack, expected_active_revision?}`: activate the revision the last activation replaced.
///
/// # Errors
/// `NOT_FOUND`, `CONFLICT_STATE` (no retained previous revision), `CONFLICT_REVISION` or malformed parameters.
pub async fn rollback_pack(
    state: &ApiState,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let name = params
        .get("pack")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("pack (the pack's name) is required", correlation))?;
    let expected = expected_active(params, correlation)?;
    let rolled = store(state)
        .rollback(
            &identity.tenant_id,
            name,
            expected.as_ref().map(|expected| expected.as_deref()),
            &actor(user_id),
            identity.correlation_id,
        )
        .await
        .map_err(|error| pack_error(error, correlation))?;
    Ok(json!({
        "pack": rolled.name,
        "pack_id": rolled.pack_id,
        "from_revision": rolled.from_revision,
        "to_revision": rolled.to_revision,
        "version_id": rolled.version_id,
    }))
}

/// `?pack=&limit=`.
#[derive(Debug, Default, Deserialize)]
pub struct PacksQuery {
    #[serde(default)]
    pack: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /v1/packs`: every pack with its active and retained revisions and, for a tenant administrator, the recent
/// install reports (state, phase, outcome and redacted diagnostics; expired ones are not returned).
///
/// # Errors
/// An authentication refusal or a store failure.
pub async fn packs(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<PacksQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let store = store(&state);
    let listed = store
        .packs(&scope.tenant_id)
        .await
        .map_err(|error| pack_error(error, "packs"))?;
    let admin = super::rbac::Authorization::check(
        state.pool(),
        &scope,
        None,
        ActionFamily::ManageTenant,
        "packs",
    )
    .await
    .is_ok();
    let reports = if admin {
        let reports = store
            .reports(
                &scope.tenant_id,
                query.pack.as_deref(),
                query.limit.unwrap_or(store.config().reports.max_listed),
            )
            .await
            .map_err(|error| pack_error(error, "packs"))?;
        Some(
            reports
                .iter()
                .map(crate::control::packs::InstallReport::to_json)
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    Ok(Json(json!({
        "packs": listed.iter().map(crate::control::packs::PackSummary::to_json).collect::<Vec<_>>(),
        "install_reports": reports,
    })))
}
