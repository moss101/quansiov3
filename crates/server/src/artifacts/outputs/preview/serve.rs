//! The HTTP side of a preview scope (DOMAIN.md §10.4 F): static files under `/p/<token>/…` and
//! suggestions at `POST /p/<token>/__quansio/suggest`.
//!
//! The token in the path is the capability. A request is answered only after the scope's authority
//! was re-resolved, the path was re-proved inside the resolved root, and the answer carries headers
//! that keep the page sandboxed and uncached.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use serde_json::{json, Value};

use super::service::{Active, Inner, ScopeError, StageError};
use super::suggest::Suggestion;

/// The path that takes suggestions, below the token.
pub const SUGGEST_PATH: &str = "__quansio/suggest";

pub(super) fn router(inner: Arc<Inner>) -> Router {
    Router::new().fallback(handle).with_state(inner)
}

async fn handle(State(inner): State<Arc<Inner>>, request: Request<Body>) -> Response {
    let mut response = route(&inner, request).await;
    harden(&inner, response.headers_mut());
    response
}

fn harden(inner: &Inner, headers: &mut HeaderMap) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if let Ok(policy) = HeaderValue::from_str(&inner.limits.csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(status, &json!({ "code": code, "message": message }))
}

fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "NOT_FOUND", "there is nothing here")
}

fn scope_error(error: &ScopeError) -> Response {
    match error {
        ScopeError::Unknown => not_found(),
        ScopeError::Forbidden => self::error(
            StatusCode::FORBIDDEN,
            "SCOPE_FORBIDDEN",
            "this scope is not yours",
        ),
        ScopeError::Lapsed { reason, scope_id } => json_response(
            StatusCode::FORBIDDEN,
            &json!({
                "code": "PREVIEW_SCOPE_LAPSED",
                "message": "this preview no longer serves",
                "details": { "reason": reason.as_str(), "scope_id": scope_id },
            }),
        ),
        ScopeError::Unavailable(_) => self::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ADAPTER_UNAVAILABLE",
            "the preview's authority could not be asked, so nothing is served",
        ),
    }
}

fn host_is_loopback(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

fn cors(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
}

async fn route(inner: &Inner, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    if !host_is_loopback(&parts.headers, inner.port) {
        return error(
            StatusCode::MISDIRECTED_REQUEST,
            "SCOPE_FORBIDDEN",
            "a preview answers only on its own loopback address",
        );
    }
    let Some(rest) = parts.uri.path().strip_prefix("/p/") else {
        return not_found();
    };
    let (token, tail) = rest.split_once('/').unwrap_or((rest, ""));
    if token.len() != inner.limits.token_bytes * 2
        || !token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return not_found();
    }
    if !rest.contains('/') {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::PERMANENT_REDIRECT;
        if let Ok(location) = HeaderValue::from_str(&format!("/p/{token}/")) {
            response.headers_mut().insert(header::LOCATION, location);
        }
        return response;
    }
    if tail == SUGGEST_PATH {
        if parts.method == Method::OPTIONS {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::NO_CONTENT;
            cors(&mut response);
            return response;
        }
        if parts.method != Method::POST {
            return method_not_allowed("POST, OPTIONS");
        }
        let mut response = suggest(inner, token, body).await;
        cors(&mut response);
        return response;
    }
    if parts.method != Method::GET && parts.method != Method::HEAD {
        return method_not_allowed("GET, HEAD");
    }
    let active = match inner.activate(token).await {
        Ok(active) => active,
        Err(error) => return scope_error(&error),
    };
    file(inner, &active, tail, parts.method == Method::HEAD).await
}

fn method_not_allowed(allow: &'static str) -> Response {
    let mut response = error(
        StatusCode::METHOD_NOT_ALLOWED,
        "VALIDATION_SCHEMA",
        "that method is not served here",
    );
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    response
}

async fn suggest(inner: &Inner, token: &str, body: Body) -> Response {
    let active = match inner.activate(token).await {
        Ok(active) => active,
        Err(error) => return scope_error(&error),
    };
    let limit = inner.limits.suggestion_max_bytes;
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "VALIDATION_BOUNDS",
            &format!("a suggestion is at most {limit} bytes"),
        );
    };
    let suggestion: Suggestion = match serde_json::from_slice(&bytes) {
        Ok(suggestion) => suggestion,
        Err(parse) => {
            return error(
                StatusCode::BAD_REQUEST,
                "VALIDATION_SCHEMA",
                &format!("not a suggestion: {parse}"),
            )
        }
    };
    match inner.stage(&active, suggestion).await {
        Ok(index) => json_response(
            StatusCode::CREATED,
            &json!({ "index": index, "scope_id": active.scope_id }),
        ),
        Err(StageError::Invalid(detail)) => {
            error(StatusCode::BAD_REQUEST, "VALIDATION_SCHEMA", &detail)
        }
        Err(StageError::TooMany(most)) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_BOUNDS",
            &format!("a scope holds at most {most} suggestions"),
        ),
        Err(StageError::Store(_)) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "the suggestion could not be stored",
        ),
    }
}

/// Why a requested path is not served.
#[derive(Debug, PartialEq, Eq)]
pub enum FileError {
    /// There is no such file, or it is not one this scope may serve.
    NotFound,
    /// The file is over the bound.
    TooLarge,
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The segments of a requested path, or `None` when the path is not one a scope serves: no empty
/// middle segment, no `.` or `..`, no dotfile or dot-directory, no separator smuggled in by
/// escaping, no control character.
#[must_use]
pub fn segments_of(tail: &str) -> Option<Vec<String>> {
    let decoded = percent_decode(tail)?;
    if decoded.contains('\0') || decoded.contains('\\') || decoded.chars().any(char::is_control) {
        return None;
    }
    let mut segments: Vec<String> = decoded.split('/').map(str::to_string).collect();
    if segments.last().is_some_and(String::is_empty) {
        segments.pop();
    }
    if segments
        .iter()
        .any(|segment| segment.is_empty() || segment.starts_with('.'))
    {
        return None;
    }
    Some(segments)
}

/// Resolve a requested path to a regular file inside `base`, which is inside `workspace_root`.
///
/// # Errors
/// [`FileError`].
pub async fn resolve_file(
    workspace_root: &Path,
    directory: &str,
    tail: &str,
    max_bytes: u64,
) -> Result<(PathBuf, u64), FileError> {
    let segments = segments_of(tail).ok_or(FileError::NotFound)?;
    let base = tokio::fs::canonicalize(workspace_root.join(directory))
        .await
        .map_err(|_| FileError::NotFound)?;
    if !base.starts_with(workspace_root) {
        return Err(FileError::NotFound);
    }
    let mut candidate = base.clone();
    for segment in &segments {
        candidate.push(segment);
    }
    let mut resolved = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|_| FileError::NotFound)?;
    let mut meta = tokio::fs::metadata(&resolved)
        .await
        .map_err(|_| FileError::NotFound)?;
    if meta.is_dir() {
        resolved.push("index.html");
        resolved = tokio::fs::canonicalize(&resolved)
            .await
            .map_err(|_| FileError::NotFound)?;
        meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|_| FileError::NotFound)?;
    }
    let Ok(relative) = resolved.strip_prefix(&base) else {
        // A link that leaves the directory is not served, and is not distinguished from a miss.
        return Err(FileError::NotFound);
    };
    if relative.components().any(|component| match component {
        Component::Normal(name) => name.to_string_lossy().starts_with('.'),
        _ => true,
    }) {
        return Err(FileError::NotFound);
    }
    if !meta.is_file() {
        return Err(FileError::NotFound);
    }
    if meta.len() > max_bytes {
        return Err(FileError::TooLarge);
    }
    Ok((resolved, meta.len()))
}

/// The media type a file is served as.
#[must_use]
pub fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

async fn file(inner: &Inner, active: &Active, tail: &str, head: bool) -> Response {
    let (path, length) = match resolve_file(
        &active.resolved.workspace_root,
        &active.directory,
        tail,
        inner.limits.max_file_bytes,
    )
    .await
    {
        Ok(found) => found,
        Err(FileError::NotFound) => return not_found(),
        Err(FileError::TooLarge) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "VALIDATION_BOUNDS",
                "that file is larger than a preview serves",
            )
        }
    };
    let body = if head {
        Body::empty()
    } else {
        match tokio::fs::read(&path).await {
            Ok(bytes) => Body::from(bytes),
            Err(_) => return not_found(),
        }
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(&path)),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_split_and_every_unsafe_shape_is_refused() {
        assert_eq!(segments_of(""), Some(vec![]));
        assert_eq!(
            segments_of("assets/app.js"),
            Some(vec!["assets".into(), "app.js".into()])
        );
        assert_eq!(segments_of("docs/"), Some(vec!["docs".into()]));
        for bad in [
            "../secret",
            "a/../../b",
            "a//b",
            ".env",
            "a/.git/config",
            "%2e%2e/secret",
            "a%2f%2e%2e%2fb",
            "a%5cb",
            "a%00b",
            "%zz",
            "a%0ab",
            "%c3%28",
        ] {
            assert_eq!(segments_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_percent_escaped_name_decodes_to_one_segment() {
        assert_eq!(segments_of("a%20b.txt"), Some(vec!["a b.txt".to_string()]));
    }

    #[test]
    fn media_types_are_chosen_by_extension() {
        assert_eq!(
            content_type(Path::new("x/index.HTML")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("a.mjs")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("a.unknown")),
            "application/octet-stream"
        );
        assert_eq!(content_type(Path::new("noext")), "application/octet-stream");
    }
}
