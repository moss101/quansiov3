//! Request tracing, the metrics endpoint and the diagnostic bundle (OPS-003).
//!
//! * [`trace_requests`] wraps every matched route: it continues the caller's `traceparent` (the
//!   desktop and web clients send one per command) or starts a trace, runs the request inside that
//!   span — so the runtime turn, its model calls and every intelligence RPC are children of it —
//!   records request metrics, and answers with the server span's `traceparent`.
//! * `GET /metrics` is operator-level Prometheus text, served only when a metrics token is
//!   configured (`QUANSIO_METRICS_TOKEN`) and presented as a bearer token.
//! * `GET /v1/diagnostics/bundle` gives a tenant administrator the tenant's recent redacted log
//!   lines, bounded.

use std::time::Instant;

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::error::{ApiError, ApiErrorCode};
use super::rbac::Authorization;
use super::ApiState;
use crate::observability::{metrics, DiagnosticBundle, Span, TraceContext, DEFAULT_BUNDLE_LIMIT, TRACEPARENT};
use crate::policy::rbac::ActionFamily;

/// Trace one HTTP request (see the module docs).
pub async fn trace_requests(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |path| path.as_str().to_string());
    let method = request.method().to_string();
    let context = request
        .headers()
        .get(TRACEPARENT)
        .and_then(|value| value.to_str().ok())
        .and_then(TraceContext::continue_from)
        .unwrap_or_else(TraceContext::root);
    let span = Span::with_context(
        context.clone(),
        "http.request",
        json!({ "method": method, "route": route }),
    );
    let started = Instant::now();
    let mut response = context.clone().scope(next.run(request)).await;
    let status = response.status().as_u16();
    let status_label = status.to_string();
    metrics::increment(
        "quansio_http_requests_total",
        "HTTP requests by route, method and status.",
        &[("route", &route), ("method", &method), ("status", &status_label)],
    );
    metrics::observe(
        "quansio_http_request_duration_seconds",
        "HTTP request latency by route.",
        &[("route", &route)],
        started.elapsed().as_secs_f64(),
    );
    if let Ok(value) = HeaderValue::from_str(&context.traceparent()) {
        response.headers_mut().insert(TRACEPARENT, value);
    }
    span.end(
        if status >= 500 { "error" } else { "ok" },
        json!({ "status": status }),
    );
    response
}

/// `GET /metrics`: Prometheus text for the operator holding the metrics token.
pub async fn metrics_endpoint(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(expected) = state.metrics_token() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    // Compare digests so the comparison time says nothing about the token.
    if Sha256::digest(presented.as_bytes()) != Sha256::digest(expected.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
        .body(Body::from(metrics::prometheus_text()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `GET /v1/diagnostics/bundle`: the tenant's recent redacted log lines, bounded.
pub async fn bundle(State(state): State<ApiState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let scope = super::TenantScope::resolve(&state, &headers).await?;
    Authorization::check(state.pool(), &scope, None, ActionFamily::ManageTenant, "diagnostics").await?;
    let bundle = DiagnosticBundle::for_tenant(&scope.tenant_id, DEFAULT_BUNDLE_LIMIT);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(bundle.to_json()))
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "diagnostics"))
}
