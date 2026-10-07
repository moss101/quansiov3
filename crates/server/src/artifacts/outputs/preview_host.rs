//! `webdev.preview_open`, `webdev.preview_renew`, `webdev.preview_close` and
//! `webdev.preview_suggestions` (ADP-006, DOMAIN.md §10.4 F).

use serde_json::{json, Value};

use super::preview::suggest::{self, Log};
use super::preview::{OpenError, PreviewService, ScopeError};
use super::support::{
    database_failure, find_output, read_version_bytes, refuse, refuse_with, success, unavailable,
    Ctx,
};
use crate::runtime::turn_loop::{HostFailure, HostOutcome};

const SCOPE: &str = "output.webdev.preview.scope";

fn rfc3339(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(String::new, |time| {
            time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
}

fn no_preview() -> HostOutcome {
    unavailable(
        "preview",
        "no preview server is composed on this deployment, so no scope can be opened",
        None,
    )
}

fn scope_failure(error: ScopeError) -> HostOutcome {
    match error {
        ScopeError::Unknown => refuse("NOT_FOUND", SCOPE, "there is no such preview scope"),
        ScopeError::Forbidden => refuse(
            "SCOPE_FORBIDDEN",
            SCOPE,
            "that preview scope was opened by someone else",
        ),
        ScopeError::Lapsed { reason, scope_id } => refuse_with(
            "PREVIEW_SCOPE_LAPSED",
            SCOPE,
            format!("the preview scope no longer serves ({})", reason.as_str()),
            Some(json!({
                "reason": reason.as_str(),
                "scope_id": scope_id,
                "suggestions_role": suggest::role_of(&scope_id),
            })),
        ),
        ScopeError::Unavailable(detail) => database_failure(&detail),
    }
}

/// `webdev.preview_open`.
///
/// # Errors
/// Never: every refusal is a [`HostOutcome`].
pub async fn open(
    ctx: &Ctx<'_>,
    service: Option<&PreviewService>,
) -> Result<HostOutcome, HostFailure> {
    let Some(service) = service else {
        return Ok(no_preview());
    };
    let directory = ctx.arg_str("directory").unwrap_or_default();
    Ok(match service.open(ctx.dispatch, directory).await {
        Ok(opened) => success(
            json!({
                "scope_id": opened.scope_id,
                "url": service.url_for(&opened.token),
                "expires_at": rfc3339(opened.expires_ms),
                "ttl_ms": opened.ttl_ms,
            }),
            Vec::new(),
            None,
        ),
        Err(OpenError::Invalid(detail)) => refuse(
            "VALIDATION_SCHEMA",
            "output.webdev.preview.directory",
            detail,
        ),
        Err(OpenError::Lapsed(reason)) => refuse(
            if reason == super::preview::Lapse::MemberRemoved {
                "SCOPE_FORBIDDEN"
            } else {
                "CONFLICT_STATE"
            },
            SCOPE,
            format!(
                "a preview cannot be opened for this run ({})",
                reason.as_str()
            ),
        ),
        Err(OpenError::Refused(detail)) => {
            refuse("CONFLICT_STATE", "output.webdev.preview.root", detail)
        }
        Err(OpenError::Unavailable(detail)) => database_failure(&detail),
        Err(OpenError::TooMany(most)) => refuse(
            "VALIDATION_BOUNDS",
            "output.webdev.preview.scopes",
            format!("at most {most} preview scopes are open at once; close one first"),
        ),
    })
}

/// `webdev.preview_renew`.
///
/// # Errors
/// Never: every refusal is a [`HostOutcome`].
pub async fn renew(
    ctx: &Ctx<'_>,
    service: Option<&PreviewService>,
) -> Result<HostOutcome, HostFailure> {
    let Some(service) = service else {
        return Ok(no_preview());
    };
    let scope_id = ctx.arg_str("scope_id").unwrap_or_default();
    Ok(match service.renew(ctx.dispatch, scope_id).await {
        Ok(renewed) => success(
            json!({
                "scope_id": scope_id,
                "expires_at": rfc3339(renewed.expires_ms),
                "lifetime_remaining_ms": renewed.lifetime_remaining_ms,
            }),
            Vec::new(),
            None,
        ),
        Err(error) => scope_failure(error),
    })
}

/// `webdev.preview_close`.
///
/// # Errors
/// Never: every refusal is a [`HostOutcome`].
pub async fn close(
    ctx: &Ctx<'_>,
    service: Option<&PreviewService>,
) -> Result<HostOutcome, HostFailure> {
    let Some(service) = service else {
        return Ok(no_preview());
    };
    let scope_id = ctx.arg_str("scope_id").unwrap_or_default();
    Ok(match service.close(ctx.dispatch, scope_id) {
        Ok(()) => success(json!({ "closed": true }), Vec::new(), None),
        Err(error) => scope_failure(error),
    })
}

/// `webdev.preview_suggestions`.
///
/// # Errors
/// Never: every refusal is a [`HostOutcome`].
pub async fn suggestions(
    ctx: &Ctx<'_>,
    service: Option<&PreviewService>,
) -> Result<HostOutcome, HostFailure> {
    let Some(service) = service else {
        return Ok(no_preview());
    };
    let scope_id = ctx.arg_str("scope_id").unwrap_or_default();
    if let Err(error) = service.check_owned(ctx.dispatch, scope_id).await {
        return Ok(scope_failure(error));
    }
    let limits = &ctx.env.config.preview;
    let found = match find_output(ctx, &suggest::role_of(scope_id)).await {
        Ok(found) => found,
        Err(outcome) => return Ok(outcome),
    };
    let log = match found {
        None => Log::new(scope_id),
        Some((_, version)) => {
            let bytes = match read_version_bytes(ctx, &version.id).await {
                Ok(bytes) => bytes,
                Err(outcome) => return Ok(outcome),
            };
            match Log::parse(&bytes, limits) {
                Ok(log) => log,
                Err(detail) => {
                    return Ok(refuse(
                        "VALIDATION_SCHEMA",
                        "artifact.format",
                        format!("the stored suggestions are not valid: {detail}"),
                    ))
                }
            }
        }
    };
    // The newest suggestions that fit the result's size bound; the count left out is reported.
    let budget = ctx
        .dispatch
        .max_output_bytes
        .saturating_sub(512)
        .min(60_000);
    let mut kept: Vec<Value> = Vec::new();
    let mut used = 0_usize;
    for stored in log.suggestions.iter().rev() {
        let value = serde_json::to_value(stored).unwrap_or(Value::Null);
        let size = value.to_string().len() + 1;
        if used + size > budget {
            break;
        }
        used += size;
        kept.push(value);
    }
    kept.reverse();
    let omitted = log.suggestions.len() - kept.len();
    Ok(success(
        json!({
            "suggestions": kept,
            "omitted": omitted,
            "trust": suggest::TRUST,
        }),
        Vec::new(),
        None,
    ))
}
