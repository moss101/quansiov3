//! Inbound hooks over HTTP (ADP-007, DOMAIN.md §13.8 A): `POST /v1/hooks/{endpoint_id}`.
//!
//! The one mutating route that carries no tenant header and no session. What authenticates a request is the endpoint's signing
//! secret (an HMAC over `"<timestamp>.<body>"`, `notify::webhooks::inbound`); what it can cause is one claimed fire of the
//! routine behind the endpoint, whose payload reaches the run only as an untrusted block. Every refusal is a typed error, and
//! none of them touches the routine.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use serde_json::{json, Value};

use super::{ApiError, ApiErrorCode, ApiState};
use crate::control::routines::hooks::{evaluate_filters, Accepted, IncomingDelivery};
use crate::notify::webhooks::inbound::{
    authenticate, resolve_endpoint, InboundRefusal, RequestHeaders, DELIVERY_ID_HEADER,
    SIGNATURE_HEADER,
};

fn refusal(error: InboundRefusal, correlation: &str) -> ApiError {
    match error {
        InboundRefusal::TooLarge { max_bytes } => ApiError::new(
            ApiErrorCode::AutomationPayloadTooLarge,
            format!("the body is over {max_bytes} bytes"),
            correlation,
        )
        .with_details(json!({ "max_bytes": max_bytes })),
        InboundRefusal::NotFound => {
            ApiError::new(ApiErrorCode::NotFound, "no such hook", correlation)
        }
        InboundRefusal::Paused => ApiError::new(
            ApiErrorCode::ConflictState,
            "this hook's routine is paused",
            correlation,
        ),
        InboundRefusal::BadSignature => ApiError::new(
            ApiErrorCode::AuthInvalidToken,
            format!("the {SIGNATURE_HEADER} header is missing or does not verify"),
            correlation,
        ),
        InboundRefusal::Stale => ApiError::new(
            ApiErrorCode::AuthExpired,
            "the signature timestamp is outside the tolerance",
            correlation,
        ),
        InboundRefusal::BadRequest(message) => {
            ApiError::new(ApiErrorCode::ValidationSchema, message, correlation)
        }
        InboundRefusal::Unavailable => ApiError::new(
            ApiErrorCode::TargetUnavailable,
            "this hook cannot verify requests right now",
            correlation,
        ),
        InboundRefusal::Internal(message) => {
            crate::observability::notice(
                crate::observability::Level::Warn,
                &format!("inbound hook: {message}"),
            );
            ApiError::new(
                ApiErrorCode::Internal,
                "the hook could not be processed",
                correlation,
            )
        }
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// `POST /v1/hooks/{endpoint_id}`: authenticate, remember, filter and claim.
pub async fn receive(
    State(state): State<ApiState>,
    Path(endpoint): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let correlation =
        quansio_core::CorrelationId::generate(&mut quansio_core::UlidGenerator::new()).to_string();
    let config = state.automation().hooks.clone();
    let refuse = |error| refusal(error, &correlation);

    // 1. Size, before anything is read or looked up: a declared length over the bound, or a stream that passes it.
    let declared = header(&headers, "content-length").and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > config.max_body_bytes) {
        return Err(refuse(InboundRefusal::TooLarge {
            max_bytes: config.max_body_bytes,
        }));
    }
    let bytes = axum::body::to_bytes(body, config.max_body_bytes + 1)
        .await
        .map_err(|_| {
            refuse(InboundRefusal::TooLarge {
                max_bytes: config.max_body_bytes,
            })
        })?;

    // 2. The endpoint, then its rate: an endpoint nobody holds costs a lookup and no memory.
    let Some(resolved) = resolve_endpoint(state.pool(), &endpoint)
        .await
        .map_err(|error| refuse(InboundRefusal::from(error)))?
    else {
        return Err(refuse(InboundRefusal::NotFound));
    };
    if !state.hook_limits().allow(&endpoint) {
        return Err(ApiError::new(
            ApiErrorCode::RateLimited,
            "this hook has exceeded its request rate",
            &correlation,
        ));
    }

    // 3. The signature, the body and the delivery id.
    let tenant = resolved.0.clone();
    let now = Utc::now();
    let keys = state.key_provider();
    let delivery = authenticate(
        state.pool(),
        &config,
        keys.as_deref(),
        resolved,
        &endpoint,
        RequestHeaders {
            signature: header(&headers, SIGNATURE_HEADER),
            delivery_id: header(&headers, DELIVERY_ID_HEADER),
        },
        &bytes,
        now,
    )
    .await
    .map_err(refuse)?;

    // 4. Filters (a boolean, nothing else of the payload is read), then the ledger.
    let trigger = delivery
        .routine
        .trigger()
        .map_err(|error| refuse(InboundRefusal::Internal(error.to_string())))?;
    let filter = evaluate_filters(&trigger.filters, &delivery.payload);
    let retention = Duration::days(config.delivery_retention_days)
        .max(Duration::seconds(config.tolerance_seconds * 2));
    let accepted = crate::notify::webhooks::inbound::ledger(state.pool(), &tenant)
        .accept(
            IncomingDelivery {
                routine_id: &delivery.routine.id,
                workspace_id: &delivery.routine.workspace_id,
                delivery_key: &delivery.delivery_key,
                signature_digest: &delivery.signature_digest,
                payload: &delivery.payload,
                filter,
                retention,
            },
            now,
        )
        .await
        .map_err(|error| refuse(InboundRefusal::Internal(error.to_string())))?;
    let (status, answer): (StatusCode, Value) = match accepted {
        Accepted::Fired(fire) => (
            StatusCode::ACCEPTED,
            json!({ "status": "accepted", "fire_id": fire.id }),
        ),
        Accepted::Filtered => (StatusCode::OK, json!({ "status": "filtered" })),
        Accepted::Duplicate => (StatusCode::OK, json!({ "status": "duplicate" })),
    };
    Ok((status, Json(answer)).into_response())
}
