//! `config/outputs.yaml` (ADP-006, DOMAIN.md §10.4): limits, thresholds and binaries for the output
//! generators, validated against `schemas/json/outputs-config.schema.json` by the repository gates.
//! Policy lives in the file, never as a source constant (D-018).

use serde::Deserialize;

/// The shipped policy.
const SHIPPED: &str = include_str!("../../../../../config/outputs.yaml");

/// The whole file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputsConfig {
    /// Always 1.
    pub version: u32,
    /// The record every output version carries.
    pub quality: Quality,
    /// The render sandbox.
    pub render: Render,
    /// Decks.
    pub deck: Deck,
    /// Printed documents.
    pub pdf: Pdf,
    /// Diagrams.
    pub diagram: Diagram,
    /// Video.
    pub video: Video,
    /// Speech-to-text.
    pub transcription: Transcription,
    /// The web-dev preview.
    pub preview: Preview,
}

/// `quality`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quality {
    /// The record's shape version.
    pub record_version: u32,
}

/// `render`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Render {
    /// How long one render may take.
    pub deadline_ms: u64,
    /// The layout viewport.
    pub viewport: Viewport,
    /// The largest screenshot kept.
    pub screenshot_max_bytes: usize,
}

/// `render.viewport`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    /// CSS pixels.
    pub width: u32,
    /// CSS pixels.
    pub height: u32,
}

/// `deck`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deck {
    /// The template version pages are built with.
    pub template_version: String,
    /// The most slides.
    pub max_slides: usize,
    /// The largest page fragment.
    pub page_max_bytes: usize,
    /// The largest shared stylesheet.
    pub css_max_bytes: usize,
    /// The largest inline image.
    pub image_max_bytes: usize,
    /// Layout slack before content overflows.
    pub overflow_tolerance_px: f64,
    /// The contrast ratio for body text.
    pub min_contrast_text: f64,
    /// The contrast ratio for large text.
    pub min_contrast_large: f64,
    /// PDF export.
    pub pdf: DeckPdf,
    /// PPTX export.
    pub pptx: DeckPptx,
}

/// `deck.pdf`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckPdf {
    /// Paper width in inches.
    pub paper_width_in: f64,
    /// Paper height in inches.
    pub paper_height_in: f64,
    /// The largest PDF.
    pub max_bytes: usize,
}

/// `deck.pptx`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckPptx {
    /// The largest package.
    pub max_bytes: usize,
    /// The most parts.
    pub max_parts: usize,
    /// The largest part.
    pub max_part_bytes: usize,
}

/// `pdf`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pdf {
    /// A printed document.
    pub document: PdfDocument,
}

/// `pdf.document`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfDocument {
    /// Paper width in inches.
    pub paper_width_in: f64,
    /// Paper height in inches.
    pub paper_height_in: f64,
    /// Margin on every side.
    pub margin_in: f64,
    /// The largest HTML body.
    pub html_max_bytes: usize,
    /// The largest PDF.
    pub max_bytes: usize,
    /// The most pages.
    pub max_pages: usize,
    /// The share of source words that must be rendered.
    pub min_text_coverage: f64,
}

/// `diagram`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagram {
    /// The most nodes.
    pub max_nodes: usize,
    /// The most edges.
    pub max_edges: usize,
    /// The largest SVG.
    pub svg_max_bytes: usize,
}

/// `video`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Video {
    /// The most operations in one batch.
    pub max_ops_per_batch: usize,
    /// The most tracks.
    pub max_tracks: usize,
    /// The most clips.
    pub max_clips: usize,
    /// The most media.
    pub max_media: usize,
    /// The most keyframes on a clip.
    pub max_keyframes_per_clip: usize,
    /// The most subtitles.
    pub max_subtitles: usize,
    /// The longest timeline.
    pub max_duration_ms: u64,
    /// The largest output width or height.
    pub max_dimension: u32,
    /// The highest frame rate.
    pub max_fps: u32,
    /// The environment variable naming the ffmpeg binary.
    pub ffmpeg_bin_env: String,
    /// How far a rendered duration may differ from the timeline's.
    pub duration_tolerance_ms: u64,
    /// Rendering.
    pub render: VideoRender,
    /// Inspection.
    pub inspect: VideoInspect,
}

/// `video.render`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoRender {
    /// How long a render job may run.
    pub timeout_ms: u64,
    /// The largest output.
    pub max_output_bytes: u64,
    /// The most media bytes a render stages.
    pub max_input_bytes: u64,
}

/// `video.inspect`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoInspect {
    /// The most frames.
    pub max_frames: usize,
    /// Contact sheet columns.
    pub columns: u32,
    /// A thumbnail's width.
    pub thumbnail_width: u32,
    /// How long an inspection may run.
    pub timeout_ms: u64,
}

/// `transcription`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcription {
    /// The gateway capability the route must have.
    pub capability: String,
    /// The largest audio file.
    pub max_audio_bytes: usize,
    /// The longest one call waits.
    pub wait_max_ms: u64,
    /// What a pending result asks the caller to wait.
    pub poll_after_ms: u64,
    /// How long a job is kept.
    pub job_ttl_ms: u64,
    /// The most segments.
    pub max_segments: usize,
    /// How far consecutive segments may overlap.
    pub segment_overlap_ms: u64,
}

/// `preview`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    /// How long a scope lives after it was opened or renewed.
    pub ttl_ms: u64,
    /// How long a scope may live in all.
    pub max_lifetime_ms: u64,
    /// The capability token's length in bytes.
    pub token_bytes: usize,
    /// The most scopes at once.
    pub max_scopes: usize,
    /// The largest file served.
    pub max_file_bytes: u64,
    /// The most suggestions per scope.
    pub max_suggestions: usize,
    /// The largest suggestion.
    pub suggestion_max_bytes: usize,
    /// The policy every response carries.
    pub csp: String,
}

impl OutputsConfig {
    /// Parse a configuration.
    ///
    /// # Errors
    /// The YAML error, when the text is not a configuration.
    pub fn from_yaml(text: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(text)
    }

    /// The shipped `config/outputs.yaml`.
    ///
    /// # Panics
    /// Only if the committed file is invalid, which the tests make impossible.
    #[must_use]
    pub fn shipped() -> Self {
        Self::from_yaml(SHIPPED).expect("config/outputs.yaml is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_policy_loads_and_is_what_the_authority_text_says() {
        let config = OutputsConfig::shipped();
        assert_eq!(config.version, 1);
        assert_eq!(
            (config.render.viewport.width, config.render.viewport.height),
            (1280, 720)
        );
        assert_eq!(config.video.max_ops_per_batch, 64);
        assert_eq!(config.preview.ttl_ms, 300_000);
        assert_eq!(config.transcription.max_audio_bytes, 25 * 1024 * 1024);
        assert!(config.preview.csp.starts_with("sandbox"));
        assert!(config.deck.min_contrast_text >= 4.5);
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_ignored() {
        let text = format!(
            "{}\nsurprise: true\n",
            include_str!("../../../../../config/outputs.yaml")
        );
        assert!(OutputsConfig::from_yaml(&text).is_err());
    }
}
