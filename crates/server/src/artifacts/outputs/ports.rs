//! What the output tools need from a worker, as ports (ADP-006, DOMAIN.md §10.4).
//!
//! A server-hosted Tool cannot lay a page out or run `ffmpeg`: those happen on an execution target.
//! The host calls a **port** for them. The port's real implementation drives the managed browser
//! (`quansio_qworkerd::browser::render`) or a supervised `ffmpeg` job on the target; when a deployment
//! has composed none, the Tool fails `ADAPTER_UNAVAILABLE` and names the adapter. No port is ever
//! stubbed in the deployable, so a missing worker is a refusal, not a plausible-looking result.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

/// What to measure on a laid-out page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeasureSpec {
    /// Only the rendered text.
    Text,
    /// Per-slide overflow, emptiness, contrast and alternative text, for `[data-slide]` pages.
    Slides {
        /// Layout slack, in CSS pixels.
        overflow_tolerance_px: f64,
        /// The contrast ratio for body text.
        min_contrast_text: f64,
        /// The contrast ratio for large text.
        min_contrast_large: f64,
    },
}

/// A PDF to print.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfSpec {
    /// Paper width in inches.
    pub paper_width_in: f64,
    /// Paper height in inches.
    pub paper_height_in: f64,
    /// Margin on every side, in inches.
    pub margin_in: f64,
    /// Whether the document's own `@page` size decides the paper.
    pub prefer_css_page_size: bool,
    /// The largest PDF accepted.
    pub max_bytes: usize,
}

/// One document to lay out in the render sandbox.
#[derive(Debug, Clone)]
pub struct RenderJob {
    /// The whole document, composed by the authority.
    pub html: String,
    /// Pinned local assets: name to (media type, bytes).
    pub assets: BTreeMap<String, (String, Vec<u8>)>,
    /// The layout viewport in CSS pixels.
    pub viewport: (u32, u32),
    /// What to measure.
    pub measure: MeasureSpec,
    /// A PDF to print, if wanted.
    pub pdf: Option<PdfSpec>,
    /// Whether to capture a PNG of the viewport.
    pub screenshot: bool,
    /// The largest screenshot accepted.
    pub max_screenshot_bytes: usize,
    /// How long the render may take.
    pub deadline_ms: u64,
}

/// A request the sandbox refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocked {
    /// The scheme.
    pub scheme: String,
    /// The host, without credentials.
    pub host: String,
    /// What it was for.
    pub resource_type: String,
}

/// What a render produced.
#[derive(Debug, Clone)]
pub struct RenderResult {
    /// The PDF, when one was asked for.
    pub pdf: Option<Vec<u8>>,
    /// The PNG, when one was asked for.
    pub screenshot: Option<Vec<u8>>,
    /// The measurement program's report (`Null` for a text-only measurement).
    pub measurements: Value,
    /// The document's rendered text.
    pub text: String,
    /// Every request the sandbox refused.
    pub blocked: Vec<Blocked>,
    /// The browser that laid it out.
    pub browser: String,
}

/// Why a port could not do its job.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortError {
    /// No such worker is composed, or its binary is missing: nothing was done.
    #[error("{adapter} is unavailable: {detail}")]
    Unavailable {
        /// `render`, `ffmpeg` or `transcription`.
        adapter: &'static str,
        /// Why, and what would make it available.
        detail: String,
    },
    /// The worker ran and failed.
    #[error("{0}")]
    Failed(String),
    /// An output passed its bound.
    #[error("{0}")]
    TooLarge(String),
}

/// The worker-side layout of one document.
#[async_trait]
pub trait RenderPort: Send + Sync {
    /// Lay the document out in the render sandbox.
    ///
    /// # Errors
    /// [`PortError`].
    async fn render(&self, job: RenderJob) -> Result<RenderResult, PortError>;
}
