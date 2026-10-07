//! Isolated document rendering on the managed browser (ADP-006, DOMAIN.md §10.4 B and C).
//!
//! A deck page, a document to print and a diagram all become pixels or a PDF the same way: the
//! trusted runtime hands this module **one HTML document** and, optionally, a few pinned local
//! assets, and the managed browser lays it out. This is not a second browser and it is not a way
//! to browse: it drives the one browser the session already uses, through the same
//! [`CdpConnection`](super::cdp::CdpConnection), and what it opens is a throwaway page in a **fresh
//! browser context**, so nothing is shared with the person's cookies, cache or tabs, and the
//! context is disposed of when the render ends, on every path.
//!
//! # What the page can reach
//!
//! * **Nothing on the network and nothing on disk.** The document is installed with
//!   `Page.setDocumentContent`, never loaded from a URL. `Fetch` is enabled for every pattern and
//!   every request the page makes is either answered from the pinned assets (only
//!   `https://assets.quansio.invalid/<name>`) or failed with `BlockedByClient` and recorded. `data:`
//!   URLs never reach `Fetch` and need not: they carry their own bytes.
//! * **No script.** The page's scripts are disabled in the renderer
//!   (`Emulation.setScriptExecutionDisabled`) and the caller's markup is expected to carry a
//!   content-security-policy that forbids them; the measurement program is evaluated by the
//!   DevTools connection, which neither is subject to.
//! * **No downloads, no popups, no dialogs**: downloads are denied for the context.
//!
//! The refusals are *recorded*, not hidden: [`RenderOutput::blocked`] lists every request the page
//! made and was refused, so a document that tried to phone home fails its quality check instead of
//! silently rendering without the picture.
//!
//! The measurement a deck page is judged by (`measure_slides.js`) is a constant of this crate: the
//! proposer of the markup cannot supply the program that grades it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::cdp::{self, CdpConnection, CdpError, PageTarget};

/// The only host a pinned asset is served from. `.invalid` never resolves, so a leak is a failure.
pub const ASSET_HOST: &str = "assets.quansio.invalid";

/// The program that measures deck pages, compiled into the worker.
const MEASURE_SLIDES: &str = include_str!("render/measure_slides.js");

/// A local file the document may use, named and typed by the trusted runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedAsset {
    /// Its media type, sent as the response's `content-type`.
    pub media_type: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// The limits a deck page is measured against (from `config/outputs.yaml`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SlideLimits {
    /// Layout slack, in CSS pixels, before content counts as overflowing.
    pub overflow_tolerance_px: f64,
    /// The smallest contrast ratio for body text.
    pub min_contrast_text: f64,
    /// The smallest contrast ratio for large text.
    pub min_contrast_large: f64,
}

/// What to read from the laid-out page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Measure {
    /// Only the page's text.
    Text,
    /// Per-slide overflow, emptiness, contrast and alternative text, for pages with `[data-slide]`.
    Slides(SlideLimits),
}

/// A PDF to print.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfJob {
    /// Paper width in inches; ignored when `prefer_css_page_size` is set and the page names a size.
    pub paper_width_in: f64,
    /// Paper height in inches.
    pub paper_height_in: f64,
    /// Margin on every side, in inches.
    pub margin_in: f64,
    /// Whether the document's own `@page { size }` decides the paper.
    pub prefer_css_page_size: bool,
    /// The largest PDF accepted, in bytes.
    pub max_bytes: usize,
}

/// One render.
#[derive(Debug, Clone)]
pub struct RenderRequest {
    /// The whole document.
    pub html: String,
    /// The pinned assets, by name.
    pub assets: BTreeMap<String, PinnedAsset>,
    /// The layout viewport in CSS pixels.
    pub viewport: (u32, u32),
    /// What to measure.
    pub measure: Measure,
    /// A PDF to print, if one is wanted.
    pub pdf: Option<PdfJob>,
    /// A PNG of the viewport to capture, if one is wanted.
    pub screenshot: bool,
    /// The largest screenshot accepted, in bytes.
    pub max_screenshot_bytes: usize,
    /// How long the whole render may take.
    pub deadline: Duration,
}

/// A request the page made and was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedRequest {
    /// The scheme (`https`, `file`, …).
    pub scheme: String,
    /// The host, without credentials; empty for a scheme with none.
    pub host: String,
    /// What the page wanted it for (`Image`, `Stylesheet`, `Document`, …).
    pub resource_type: String,
}

/// What a render produced.
#[derive(Debug, Clone)]
pub struct RenderOutput {
    /// The PDF, when one was asked for.
    pub pdf: Option<Vec<u8>>,
    /// The PNG, when one was asked for.
    pub screenshot: Option<Vec<u8>>,
    /// The measurement program's report (`Null` for [`Measure::Text`]).
    pub measurements: Value,
    /// The document's rendered text (`document.body.innerText`).
    pub text: String,
    /// Every request refused.
    pub blocked: Vec<BlockedRequest>,
    /// The browser's version string, so a record says what laid the page out.
    pub browser: String,
}

/// Why a render did not produce an output.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The browser could not be reached or failed a call.
    #[error("the render browser failed: {0}")]
    Browser(#[from] CdpError),
    /// The render did not finish inside its deadline.
    #[error("the render did not finish within {0:?}")]
    Deadline(Duration),
    /// An output passed its bound.
    #[error("the {what} is {bytes} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// Which output.
        what: &'static str,
        /// Its size.
        bytes: usize,
        /// The bound.
        limit: usize,
    },
    /// The browser's answer was not what the protocol describes.
    #[error("the render browser answered unexpectedly: {0}")]
    Unexpected(String),
    /// A pinned asset's name is not an allowed name.
    #[error("{0:?} is not a valid pinned asset name")]
    AssetName(String),
}

/// Whether a pinned asset name is a plain file name: lower-case letters, digits, `.`, `-`, `_`.
#[must_use]
pub fn valid_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// Render one document on the browser listening at `port`.
///
/// # Errors
/// [`RenderError`] when the browser is unreachable, a call fails, the deadline passes or an output
/// is over its bound. The throwaway context is disposed of on every path.
pub async fn render(port: u16, request: &RenderRequest) -> Result<RenderOutput, RenderError> {
    for name in request.assets.keys() {
        if !valid_asset_name(name) {
            return Err(RenderError::AssetName(name.clone()));
        }
    }
    let deadline = request.deadline;
    match tokio::time::timeout(deadline, render_inner(port, request)).await {
        Ok(result) => result,
        Err(_) => Err(RenderError::Deadline(deadline)),
    }
}

async fn render_inner(port: u16, request: &RenderRequest) -> Result<RenderOutput, RenderError> {
    // The browser-level connection creates and disposes the isolated context.
    let browser_socket = cdp::discover(port).await?;
    let browser = cdp::connect(PageTarget {
        id: "browser".to_string(),
        title: String::new(),
        url: String::new(),
        websocket_url: browser_socket,
    })
    .await?;
    let version = browser
        .call("Browser.getVersion", json!({}))
        .await
        .ok()
        .and_then(|v| v.get("product").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let context = browser
        .call(
            "Target.createBrowserContext",
            json!({ "disposeOnDetach": true }),
        )
        .await?;
    let context_id = context
        .get("browserContextId")
        .and_then(Value::as_str)
        .ok_or_else(|| RenderError::Unexpected("no browser context id".to_string()))?
        .to_string();

    let outcome = render_in_context(port, &browser, &context_id, request, version).await;

    // Always dispose, whatever happened: the context holds the page, its cache and its storage.
    let _ = browser
        .call(
            "Target.disposeBrowserContext",
            json!({ "browserContextId": context_id }),
        )
        .await;
    outcome
}

async fn render_in_context(
    port: u16,
    browser: &CdpConnection,
    context_id: &str,
    request: &RenderRequest,
    version: String,
) -> Result<RenderOutput, RenderError> {
    let created = browser
        .call(
            "Target.createTarget",
            json!({ "url": "about:blank", "browserContextId": context_id }),
        )
        .await?;
    let target_id = created
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| RenderError::Unexpected("no target id".to_string()))?
        .to_string();
    let page = cdp::connect(PageTarget {
        id: target_id.clone(),
        title: String::new(),
        url: "about:blank".to_string(),
        websocket_url: format!("ws://127.0.0.1:{port}/devtools/page/{target_id}"),
    })
    .await?;
    let page = Arc::new(page);
    let blocked = Arc::new(Mutex::new(Vec::<BlockedRequest>::new()));

    let result = drive_page(&page, request, &blocked, version).await;

    let _ = browser
        .call("Target.closeTarget", json!({ "targetId": target_id }))
        .await;
    result
}

async fn drive_page(
    page: &Arc<CdpConnection>,
    request: &RenderRequest,
    blocked: &Arc<Mutex<Vec<BlockedRequest>>>,
    version: String,
) -> Result<RenderOutput, RenderError> {
    page.call("Page.enable", json!({})).await?;
    page.call(
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": request.viewport.0,
            "height": request.viewport.1,
            "deviceScaleFactor": 1,
            "mobile": false,
        }),
    )
    .await?;
    let _ = page
        .call("Browser.setDownloadBehavior", json!({ "behavior": "deny" }))
        .await;

    // Intercept before anything can be requested.
    let interceptor = spawn_interceptor(
        Arc::clone(page),
        request.assets.clone(),
        Arc::clone(blocked),
    );
    page.call(
        "Fetch.enable",
        json!({ "patterns": [{ "urlPattern": "*" }] }),
    )
    .await?;
    page.call(
        "Emulation.setScriptExecutionDisabled",
        json!({ "value": true }),
    )
    .await?;

    let outcome = produce(page, request, blocked, version).await;
    interceptor.abort();
    outcome
}

async fn produce(
    page: &Arc<CdpConnection>,
    request: &RenderRequest,
    blocked: &Arc<Mutex<Vec<BlockedRequest>>>,
    version: String,
) -> Result<RenderOutput, RenderError> {
    let tree = page.call("Page.getFrameTree", json!({})).await?;
    let frame_id = tree
        .pointer("/frameTree/frame/id")
        .and_then(Value::as_str)
        .ok_or_else(|| RenderError::Unexpected("no frame id".to_string()))?
        .to_string();
    page.call(
        "Page.setDocumentContent",
        json!({ "frameId": frame_id, "html": request.html }),
    )
    .await?;
    settle(page).await?;

    let (measurements, text) = match request.measure {
        Measure::Text => (Value::Null, evaluate_text(page).await?),
        Measure::Slides(limits) => {
            let program = format!(
                "{MEASURE_SLIDES}({})",
                serde_json::to_string(&limits)
                    .map_err(|e| RenderError::Unexpected(e.to_string()))?
            );
            let report = evaluate(page, &program).await?;
            let text = report
                .get("document_text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (report, text)
        }
    };

    let screenshot = if request.screenshot {
        let shot = page
            .call(
                "Page.captureScreenshot",
                json!({
                    "format": "png",
                    "clip": {
                        "x": 0, "y": 0,
                        "width": request.viewport.0, "height": request.viewport.1, "scale": 1,
                    },
                }),
            )
            .await?;
        let bytes = decode(shot.get("data").and_then(Value::as_str))?;
        if bytes.len() > request.max_screenshot_bytes {
            return Err(RenderError::TooLarge {
                what: "screenshot",
                bytes: bytes.len(),
                limit: request.max_screenshot_bytes,
            });
        }
        Some(bytes)
    } else {
        None
    };

    let pdf = if let Some(job) = &request.pdf {
        let printed = page
            .call_within(
                "Page.printToPDF",
                json!({
                    "printBackground": true,
                    "preferCSSPageSize": job.prefer_css_page_size,
                    "paperWidth": job.paper_width_in,
                    "paperHeight": job.paper_height_in,
                    "marginTop": job.margin_in,
                    "marginBottom": job.margin_in,
                    "marginLeft": job.margin_in,
                    "marginRight": job.margin_in,
                    "displayHeaderFooter": false,
                    "transferMode": "ReturnAsBase64",
                }),
                Duration::from_secs(120),
            )
            .await?;
        let bytes = decode(printed.get("data").and_then(Value::as_str))?;
        if bytes.len() > job.max_bytes {
            return Err(RenderError::TooLarge {
                what: "PDF",
                bytes: bytes.len(),
                limit: job.max_bytes,
            });
        }
        Some(bytes)
    } else {
        None
    };

    let blocked = blocked.lock().await.clone();
    Ok(RenderOutput {
        pdf,
        screenshot,
        measurements,
        text,
        blocked,
        browser: version,
    })
}

/// Wait for fonts and images the document may use: a fixed number of short polls of
/// `document.readyState`, not a sleep, and bounded by the render's own deadline.
async fn settle(page: &CdpConnection) -> Result<(), RenderError> {
    for _ in 0..200 {
        let ready = evaluate(page, "document.readyState === 'complete'").await?;
        if ready.as_bool() == Some(true) {
            let _ = evaluate(
                page,
                "(async () => { if (document.fonts && document.fonts.ready) await document.fonts.ready; \
                 await Promise.all(Array.from(document.images).map(i => i.decode().catch(() => undefined))); return true; })()",
            )
            .await;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(RenderError::Unexpected(
        "the document never reached readyState complete".to_string(),
    ))
}

async fn evaluate_text(page: &CdpConnection) -> Result<String, RenderError> {
    let value = evaluate(page, "document.body ? document.body.innerText : ''").await?;
    Ok(value.as_str().unwrap_or_default().to_string())
}

async fn evaluate(page: &CdpConnection, expression: &str) -> Result<Value, RenderError> {
    let result = page
        .call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )
        .await?;
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(RenderError::Unexpected(format!(
            "the measurement raised: {}",
            exception
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| exception.get("text").and_then(Value::as_str))
                .unwrap_or("unknown")
        )));
    }
    Ok(result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null))
}

fn decode(data: Option<&str>) -> Result<Vec<u8>, RenderError> {
    let encoded =
        data.ok_or_else(|| RenderError::Unexpected("an output came back empty".to_string()))?;
    super::decode_base64_bytes(encoded)
        .ok_or_else(|| RenderError::Unexpected("an output was not base64".to_string()))
}

/// Answer every paused request: a pinned asset from memory, anything else refused and recorded.
fn spawn_interceptor(
    page: Arc<CdpConnection>,
    assets: BTreeMap<String, PinnedAsset>,
    blocked: Arc<Mutex<Vec<BlockedRequest>>>,
) -> tokio::task::JoinHandle<()> {
    let mut events = page.subscribe();
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if event.method != "Fetch.requestPaused" {
                continue;
            }
            let Some(request_id) = event.params.get("requestId").and_then(Value::as_str) else {
                continue;
            };
            let url = event
                .params
                .pointer("/request/url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let resource_type = event
                .params
                .get("resourceType")
                .and_then(Value::as_str)
                .unwrap_or("Other");
            if let Some(asset) = pinned(url, &assets) {
                let _ = page
                    .call(
                        "Fetch.fulfillRequest",
                        json!({
                            "requestId": request_id,
                            "responseCode": 200,
                            "responseHeaders": [
                                { "name": "content-type", "value": asset.media_type },
                                { "name": "cache-control", "value": "no-store" },
                            ],
                            "body": base64_encode(&asset.bytes),
                        }),
                    )
                    .await;
            } else {
                let (scheme, host) = split_url(url);
                blocked.lock().await.push(BlockedRequest {
                    scheme,
                    host,
                    resource_type: resource_type.to_string(),
                });
                let _ = page
                    .call(
                        "Fetch.failRequest",
                        json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                    )
                    .await;
            }
        }
    })
}

fn pinned<'a>(url: &str, assets: &'a BTreeMap<String, PinnedAsset>) -> Option<&'a PinnedAsset> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    if host != ASSET_HOST {
        return None;
    }
    let name = path.split(['?', '#']).next()?;
    assets.get(name)
}

fn split_url(url: &str) -> (String, String) {
    let Some((scheme, rest)) = url.split_once(':') else {
        return (String::new(), String::new());
    };
    let authority = rest
        .strip_prefix("//")
        .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    // Never keep credentials a hostile URL embeds.
    let host = authority.rsplit('@').next().unwrap_or_default();
    (scheme.to_ascii_lowercase(), host.to_ascii_lowercase())
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_are_plain_file_names() {
        assert!(valid_asset_name("logo.png"));
        assert!(valid_asset_name("a-b_c.1.svg"));
        for bad in [
            "",
            ".hidden",
            "../x",
            "a/b",
            "UP.png",
            "a b",
            "x?y",
            &"a".repeat(65),
        ] {
            assert!(!valid_asset_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_the_pinned_host_serves_a_pinned_asset() {
        let mut assets = BTreeMap::new();
        assets.insert(
            "logo.png".to_string(),
            PinnedAsset {
                media_type: "image/png".to_string(),
                bytes: vec![1],
            },
        );
        assert!(pinned("https://assets.quansio.invalid/logo.png", &assets).is_some());
        assert!(pinned("https://assets.quansio.invalid/logo.png?x=1", &assets).is_some());
        assert!(pinned("https://assets.quansio.invalid/other.png", &assets).is_none());
        assert!(pinned("https://example.com/logo.png", &assets).is_none());
        assert!(pinned(
            "https://assets.quansio.invalid.example.com/logo.png",
            &assets
        )
        .is_none());
        assert!(pinned("http://assets.quansio.invalid/logo.png", &assets).is_none());
        assert!(pinned("file:///etc/hosts", &assets).is_none());
    }

    #[test]
    fn a_blocked_url_is_recorded_without_credentials() {
        assert_eq!(
            split_url("https://user:secret@Example.com:8443/path?q=1"),
            ("https".to_string(), "example.com:8443".to_string())
        );
        assert_eq!(
            split_url("file:///etc/hosts"),
            ("file".to_string(), String::new())
        );
        assert_eq!(split_url("garbage"), (String::new(), String::new()));
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
