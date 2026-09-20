//! The web administration and review app as static assets (APP-012).
//!
//! `quansio-server` serves the built `apps/web` bundle under `/app/` from the directory
//! `QUANSIO_WEB_ROOT` names (a CDN can serve the same directory). The page is a client
//! of the public API exactly like the desktop: it gets no host-privileged API, no
//! cookie, no session of its own — a bearer session from `/v1/auth/*`, carried by the
//! page's own fetches. What the server adds is the delivery: a locked
//! Content-Security-Policy (scripts, styles and connections to this origin only, no
//! frames, no inline script), `nosniff`, no directory listing, and a path that can
//! never leave the root. A client-side route (`/app/audit`) falls back to `index.html`;
//! a missing asset is a typed `NOT_FOUND`.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as RoutePath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;

use super::{ApiError, ApiErrorCode, ApiState};

/// Largest asset served (the bundle is ~1 MB; anything larger is a mistake).
pub const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024;

/// The policy every page response carries.
pub const WEB_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                           font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
                           form-action 'self'; object-src 'none'";

/// Where the bundle lives.
#[derive(Debug, Clone)]
pub struct WebRoot {
    root: Arc<PathBuf>,
}

impl WebRoot {
    /// A root directory; it must exist and contain `index.html`.
    ///
    /// # Errors
    /// Returns a message when the directory or its `index.html` is missing.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root: PathBuf = root.into();
        let root = root
            .canonicalize()
            .map_err(|error| format!("web root {}: {error}", root.display()))?;
        if !root.join("index.html").is_file() {
            return Err(format!("web root {} has no index.html", root.display()));
        }
        Ok(Self {
            root: Arc::new(root),
        })
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Resolve a request path to a file under the root, or `None` when the path would
    /// leave the root, names a directory, or is not there. Only normal components are
    /// accepted: no `..`, no absolute paths, no prefixes.
    #[must_use]
    pub fn resolve(&self, request_path: &str) -> Option<PathBuf> {
        let mut resolved = (*self.root).clone();
        for component in Path::new(request_path).components() {
            match component {
                Component::Normal(part) => resolved.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        let canonical = resolved.canonicalize().ok()?;
        if !canonical.starts_with(&*self.root) || !canonical.is_file() {
            return None;
        }
        Some(canonical)
    }
}

/// The media type an asset is served as, by extension; unknown extensions are octets.
#[must_use]
pub fn media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Whether a client-side route may fall back to `index.html`: no extension, so it was
/// never an asset request.
#[must_use]
pub fn is_client_route(request_path: &str) -> bool {
    !request_path.rsplit('/').next().unwrap_or("").contains('.')
}

fn missing(what: &str) -> ApiError {
    ApiError::new(
        ApiErrorCode::NotFound,
        format!("{what} is not served here"),
        "web",
    )
}

/// Whether the path tries to climb: refused before any lookup, never a fallback.
#[must_use]
pub fn escapes(request_path: &str) -> bool {
    Path::new(request_path)
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
}

async fn serve(root: &WebRoot, request_path: &str) -> Result<Response, ApiError> {
    if escapes(request_path) {
        return Err(missing(request_path));
    }
    let file = match root.resolve(request_path) {
        Some(file) => file,
        None if is_client_route(request_path) => root.root.join("index.html"),
        None => return Err(missing(request_path)),
    };
    let size = std::fs::metadata(&file)
        .map(|metadata| metadata.len())
        .map_err(|_| missing(request_path))?;
    if size > MAX_ASSET_BYTES {
        return Err(missing(request_path));
    }
    let bytes =
        tokio::task::spawn_blocking(move || std::fs::read(&file).map(|bytes| (file, bytes)))
            .await
            .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "web"))?
            .map_err(|_| missing(request_path))?;
    let (file, bytes) = bytes;
    let kind = media_type(&file);
    // Assets are not content-hashed, so every response revalidates: a deploy is visible
    // on the next load rather than after a cache window.
    let cache = "no-cache";
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, kind)
        .header(header::CACHE_CONTROL, cache)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, WEB_CSP)
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::X_FRAME_OPTIONS, "DENY")
        .header(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()))
        .body(Body::from(bytes))
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), "web"))
}

fn root_of(state: &ApiState) -> Result<&WebRoot, ApiError> {
    state.web_root().ok_or_else(|| missing("the web app"))
}

/// `GET /app` and `GET /app/`: the page.
pub async fn index(State(state): State<ApiState>) -> Result<Response, ApiError> {
    serve(root_of(&state)?, "index.html").await
}

/// `GET /app/*path`: an asset, or the page for a client-side route.
pub async fn asset(
    State(state): State<ApiState>,
    RoutePath(path): RoutePath<String>,
) -> Result<Response, ApiError> {
    serve(root_of(&state)?, &path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_never_leaves_the_root_and_routes_fall_back_to_the_page() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("index.html"), "<!doctype html>").expect("index");
        std::fs::create_dir(dir.path().join("assets")).expect("assets");
        std::fs::write(dir.path().join("assets/app.js"), "//").expect("js");
        std::fs::write(
            dir.path()
                .parent()
                .expect("parent")
                .join("outside-web-root.txt"),
            "x",
        )
        .ok();
        let root = WebRoot::new(dir.path()).expect("root");
        assert!(root.resolve("index.html").is_some());
        assert!(root.resolve("assets/app.js").is_some());
        assert!(
            root.resolve("assets").is_none(),
            "a directory is not an asset"
        );
        assert!(root.resolve("../outside-web-root.txt").is_none());
        assert!(root.resolve("/etc/passwd").is_none());
        assert!(root.resolve("assets/../../outside-web-root.txt").is_none());
        assert!(escapes("../x"));
        assert!(escapes("assets/../../x"));
        assert!(escapes("/etc/passwd"));
        assert!(!escapes("assets/app.js"));
        assert!(is_client_route("audit"));
        assert!(is_client_route("approvals/apr_1"));
        assert!(!is_client_route("assets/missing.js"));
        assert_eq!(media_type(Path::new("a.css")), "text/css; charset=utf-8");
        assert_eq!(media_type(Path::new("a.bin")), "application/octet-stream");
        assert!(
            WebRoot::new(dir.path().join("assets")).is_err(),
            "no index.html"
        );
    }
}
