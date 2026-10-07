//! Decks: the slide source, the pages the plane renders from it, and the completion gate (ADP-006,
//! DOMAIN.md §10.4 B).
//!
//! This module is pure: it parses a presentation, derives each slide's digest and the lines a page
//! or a package must say, composes the one trusted page shell, and decides the **deck gate**. The
//! tool host (`deck_host.rs`) does the I/O around it.
//!
//! # The gate
//!
//! A deck is deliverable only when every page of its *current* version is `ready`. The pages are
//! recorded in a tool-owned Artifact (the deck pages) by `deck.render`, per slide, with the digest of
//! the source the page was rendered from. When the deck is edited, the gate does not trust a stored
//! state: it compares digests, so a slide whose source changed is `edited` and a slide with no page
//! is `pending` whatever the manifest says about its neighbours. `failed_quality` is a state a
//! render writes; the only way out of it is a render whose measurements pass.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::html;

/// The pages Artifact's media type.
pub const PAGES_MEDIA_TYPE: &str = "application/vnd.quansio.deck-pages+json";
/// The PDF export's media type.
pub const PDF_MEDIA_TYPE: &str = "application/pdf";
/// The PPTX export's media type.
pub const PPTX_MEDIA_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";
/// The role prefix of the pages Artifact (`deck_pages:<deck artifact id>`).
pub const PAGES_ROLE: &str = "deck_pages";
/// The role prefix of a PDF export.
pub const PDF_ROLE: &str = "deck_pdf";
/// The role prefix of a PPTX export.
pub const PPTX_ROLE: &str = "deck_pptx";

/// The slide layouts the templates build.
pub const LAYOUTS: &[&str] = &[
    "title",
    "section",
    "content",
    "two_column",
    "image",
    "quote",
    "chart",
];

const SLIDE_KEYS: &[&str] = &["title", "body", "notes", "layout", "alt", "image", "chart"];
const CHART_KEYS: &[&str] = &["kind", "labels", "values", "unit"];
const MAX_CHART_POINTS: usize = 24;

/// A bar or line chart carried in a slide.
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    /// `bar` or `line`.
    pub kind: String,
    /// One label per value.
    pub labels: Vec<String>,
    /// The values.
    pub values: Vec<f64>,
    /// An optional unit.
    pub unit: Option<String>,
}

/// One slide of a presentation source.
#[derive(Debug, Clone, PartialEq)]
pub struct SlideSource {
    /// Its title.
    pub title: String,
    /// Its body text.
    pub body: String,
    /// Its speaker notes.
    pub notes: String,
    /// Its layout.
    pub layout: String,
    /// The alternative text of its image or chart.
    pub alt: Option<String>,
    /// An inline image.
    pub image: Option<String>,
    /// A chart.
    pub chart: Option<Chart>,
    /// The slide as the source mapping, kept for the digest.
    mapping: Value,
}

/// A presentation source.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentationSource {
    /// The slides, in order.
    pub slides: Vec<SlideSource>,
}

fn text_field(map: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{key} must be text")),
    }
}

impl SlideSource {
    fn parse(index: usize, value: &Value, image_max_bytes: usize) -> Result<Self, String> {
        let label = format!("slide {index}");
        let map = value
            .as_object()
            .ok_or_else(|| format!("{label} is not an object"))?;
        if let Some(extra) = map.keys().find(|key| !SLIDE_KEYS.contains(&key.as_str())) {
            return Err(format!("{label} has unknown keys: {extra}"));
        }
        let title = text_field(map, "title")
            .map_err(|e| format!("{label}: {e}"))?
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| format!("{label} needs a title"))?;
        let body = text_field(map, "body")
            .map_err(|e| format!("{label}: {e}"))?
            .unwrap_or_default();
        let notes = text_field(map, "notes")
            .map_err(|e| format!("{label}: {e}"))?
            .unwrap_or_default();
        let layout = text_field(map, "layout")
            .map_err(|e| format!("{label}: {e}"))?
            .unwrap_or_else(|| "content".to_string());
        if !LAYOUTS.contains(&layout.as_str()) {
            return Err(format!("{label}: {layout:?} is not a layout"));
        }
        let alt = text_field(map, "alt").map_err(|e| format!("{label}: {e}"))?;
        if let Some(alt) = &alt {
            if alt.chars().count() > 300 {
                return Err(format!("{label}: alt is at most 300 characters"));
            }
        }
        let image = text_field(map, "image").map_err(|e| format!("{label}: {e}"))?;
        let chart = match map.get("chart") {
            None => None,
            Some(value) => Some(parse_chart(value).map_err(|e| format!("{label}: {e}"))?),
        };
        match layout.as_str() {
            "image" => {
                let Some(image) = &image else {
                    return Err(format!("{label}: an image layout needs an image"));
                };
                if !html::is_inline_image(image) {
                    return Err(format!(
                        "{label}: image must be an inline png, jpeg, gif or webp data: URI"
                    ));
                }
                let base64 = image.split(";base64,").nth(1).unwrap_or_default();
                if base64.len() / 4 * 3 > image_max_bytes {
                    return Err(format!(
                        "{label}: the image is over {image_max_bytes} bytes"
                    ));
                }
                if alt.as_deref().is_none_or(|alt| alt.trim().is_empty()) {
                    return Err(format!("{label}: an image layout needs alt text"));
                }
                if chart.is_some() {
                    return Err(format!("{label}: chart is only for the chart layout"));
                }
            }
            "chart" => {
                if chart.is_none() {
                    return Err(format!("{label}: a chart layout needs a chart"));
                }
                if alt.as_deref().is_none_or(|alt| alt.trim().is_empty()) {
                    return Err(format!("{label}: a chart layout needs alt text"));
                }
                if image.is_some() {
                    return Err(format!("{label}: image is only for the image layout"));
                }
            }
            _ => {
                if image.is_some() || chart.is_some() || alt.is_some() {
                    return Err(format!(
                        "{label}: image, chart and alt belong to the image and chart layouts"
                    ));
                }
            }
        }
        Ok(Self {
            title,
            body,
            notes,
            layout,
            alt,
            image,
            chart,
            mapping: value.clone(),
        })
    }

    /// The digest the slide's page is bound to: SHA-256 of its source mapping with sorted keys,
    /// compact separators and unescaped non-ASCII text.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(canonical(&self.mapping).as_bytes());
        hex(&hasher.finalize())
    }

    /// The lines of body text a page or a package must carry.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        body_lines(&self.body)
    }
}

fn parse_chart(value: &Value) -> Result<Chart, String> {
    let map = value.as_object().ok_or("chart must be an object")?;
    if let Some(extra) = map.keys().find(|key| !CHART_KEYS.contains(&key.as_str())) {
        return Err(format!("chart has unknown keys: {extra}"));
    }
    let kind = map
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| matches!(*kind, "bar" | "line"))
        .ok_or("chart kind must be bar or line")?
        .to_string();
    let labels: Vec<String> = map
        .get("labels")
        .and_then(Value::as_array)
        .ok_or("chart labels must be a list")?
        .iter()
        .map(|label| {
            label
                .as_str()
                .map(str::to_string)
                .filter(|label| !label.trim().is_empty() && label.chars().count() <= 40)
                .ok_or("a chart label is 1 to 40 characters")
        })
        .collect::<Result<_, _>>()?;
    let values: Vec<f64> = map
        .get("values")
        .and_then(Value::as_array)
        .ok_or("chart values must be a list")?
        .iter()
        .map(|number| {
            number
                .as_f64()
                .filter(|v| v.is_finite() && v.abs() < 1e12 && (*v == 0.0 || v.abs() >= 1e-4))
                .ok_or("a chart value is a number of magnitude 0.0001 to 1e12")
        })
        .collect::<Result<_, _>>()?;
    if labels.is_empty() || labels.len() > MAX_CHART_POINTS || labels.len() != values.len() {
        return Err(format!(
            "a chart has 1 to {MAX_CHART_POINTS} points, one label for each value"
        ));
    }
    let unit = match map.get("unit") {
        None => None,
        Some(Value::String(unit)) if unit.chars().count() <= 12 => Some(unit.clone()),
        Some(_) => return Err("chart unit is text of at most 12 characters".to_string()),
    };
    Ok(Chart {
        kind,
        labels,
        values,
        unit,
    })
}

/// Parse and validate a presentation source (DOMAIN.md §10.1 and §10.4 B).
///
/// # Errors
/// A sentence naming what is wrong.
pub fn parse_presentation(
    bytes: &[u8],
    image_max_bytes: usize,
) -> Result<PresentationSource, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "presentation is not JSON".to_string())?;
    let map = value
        .as_object()
        .ok_or("presentation must be a JSON object")?;
    if let Some(extra) = map
        .keys()
        .find(|key| !matches!(key.as_str(), "schema_version" | "slides"))
    {
        return Err(format!("presentation has unknown keys: {extra}"));
    }
    if map.get("schema_version").and_then(Value::as_str) != Some("v1") {
        return Err("presentation schema_version must be v1".to_string());
    }
    let slides = map
        .get("slides")
        .and_then(Value::as_array)
        .ok_or("presentation slides must be a list")?;
    if slides.is_empty() {
        return Err("presentation needs at least one slide".to_string());
    }
    Ok(PresentationSource {
        slides: slides
            .iter()
            .enumerate()
            .map(|(index, slide)| SlideSource::parse(index, slide, image_max_bytes))
            .collect::<Result<_, _>>()?,
    })
}

/// Compact JSON with sorted keys and unescaped non-ASCII text.
#[must_use]
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{}:{}", Value::String(key.clone()), canonical(&map[key])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// Lower-case hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// SHA-256 of text, in hex.
#[must_use]
pub fn sha256_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// The lines of a slide body a page must say: each non-blank line, a bullet's `- ` and a quote's
/// leading dash removed, the `---` column break dropped.
#[must_use]
pub fn body_lines(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "---")
        .map(|line| {
            line.strip_prefix("- ")
                .or_else(|| line.strip_prefix("— "))
                .unwrap_or(line)
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect()
}

/// The state of one slide's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlideState {
    /// No page has been rendered for it.
    Pending,
    /// Its source changed since its page was rendered.
    Edited,
    /// Its page was rendered and a check failed.
    FailedQuality,
    /// Its page was rendered from this source and every check held.
    Ready,
}

impl SlideState {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Edited => "edited",
            Self::FailedQuality => "failed_quality",
            Self::Ready => "ready",
        }
    }
}

/// One check as the pages Artifact keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageCheck {
    /// The rule.
    pub rule: String,
    /// Whether it held.
    pub ok: bool,
    /// What it found.
    pub detail: String,
}

/// One slide's entry in the pages Artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageEntry {
    /// The slide's position.
    pub index: usize,
    /// The digest of the source this entry is for.
    pub source_digest: String,
    /// The state.
    pub state: SlideState,
    /// The slide's section element, as rendered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
    /// The digest of the page document the browser laid out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_digest: Option<String>,
    /// The checks that ran.
    #[serde(default)]
    pub checks: Vec<PageCheck>,
    /// The digest of the text the browser rendered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_digest: Option<String>,
    /// The screenshots recorded as evidence.
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

/// The deck version the pages are for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckRef {
    /// The presentation Artifact.
    pub artifact_id: String,
    /// The version the pages were rendered from.
    pub version_id: String,
    /// That version's content digest.
    pub content_digest: String,
    /// How many slides it had.
    pub slide_count: usize,
}

/// The pages Artifact's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckPages {
    /// Always `v1`.
    pub schema_version: String,
    /// The template version the plane built the pages with.
    pub template_version: String,
    /// The deck version.
    pub deck: DeckRef,
    /// The shared stylesheet.
    pub css: String,
    /// One entry per slide.
    pub slides: Vec<PageEntry>,
}

impl DeckPages {
    /// Parse and shape-check stored pages.
    ///
    /// # Errors
    /// A sentence naming what is wrong.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let pages: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if pages.schema_version != "v1" {
            return Err("deck pages schema_version must be v1".to_string());
        }
        if pages.deck.slide_count != pages.slides.len() || pages.slides.is_empty() {
            return Err("the pages must hold one entry per slide of the deck".to_string());
        }
        for (position, entry) in pages.slides.iter().enumerate() {
            if entry.index != position {
                return Err(format!("entry {position} is for slide {}", entry.index));
            }
            if entry.source_digest.len() != 64 {
                return Err(format!("slide {position}: source_digest must be a SHA-256"));
            }
            let rendered = matches!(entry.state, SlideState::Ready | SlideState::FailedQuality);
            if rendered && (entry.fragment.is_none() || entry.page_digest.is_none()) {
                return Err(format!(
                    "slide {position} is {} with no page",
                    entry.state.as_str()
                ));
            }
            if entry.state == SlideState::Ready && entry.checks.iter().any(|check| !check.ok) {
                return Err(format!("slide {position} is ready with a failing check"));
            }
            if entry.state == SlideState::FailedQuality && entry.checks.iter().all(|check| check.ok)
            {
                return Err(format!("slide {position} failed with no failing check"));
            }
        }
        Ok(pages)
    }

    /// The canonical bytes the Artifact stores.
    ///
    /// # Panics
    /// Never: the structure serializes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("deck pages serialize")
    }
}

/// One slide's standing against the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlideStanding {
    /// The slide's position.
    pub index: usize,
    /// Its state against the current source.
    pub state: SlideState,
    /// The rules that failed, for `failed_quality`.
    pub failed_rules: Vec<String>,
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// Whether the deck may be delivered.
    pub deliverable: bool,
    /// Every slide's standing.
    pub slides: Vec<SlideStanding>,
    /// Whether the pages are for another version of the deck than the current one.
    pub stale: bool,
}

impl Gate {
    /// The slides that stand in the way.
    #[must_use]
    pub fn blockers(&self) -> Vec<&SlideStanding> {
        self.slides
            .iter()
            .filter(|slide| slide.state != SlideState::Ready)
            .collect()
    }
}

/// Decide the gate for a deck's current version against the pages recorded for it.
///
/// `pages` is `None` when there are none (or they are not the authority's own); then every slide is
/// pending. Otherwise each slide's state is read from its entry **only if** the entry was rendered
/// from the slide's current source; a changed source is `edited` and a slide beyond the pages is
/// `pending`, whatever the entry said.
#[must_use]
pub fn gate(
    deck: &PresentationSource,
    current_version_id: &str,
    pages: Option<&DeckPages>,
    template_version: &str,
) -> Gate {
    let digests: Vec<String> = deck.slides.iter().map(SlideSource::digest).collect();
    let mut slides = Vec::with_capacity(digests.len());
    let usable = pages.filter(|pages| pages.template_version == template_version);
    for (index, digest) in digests.iter().enumerate() {
        let standing = match usable.and_then(|pages| pages.slides.get(index)) {
            None => SlideStanding {
                index,
                state: SlideState::Pending,
                failed_rules: Vec::new(),
            },
            Some(entry) if entry.source_digest != *digest => SlideStanding {
                index,
                state: SlideState::Edited,
                failed_rules: Vec::new(),
            },
            Some(entry) => SlideStanding {
                index,
                state: entry.state,
                failed_rules: entry
                    .checks
                    .iter()
                    .filter(|check| !check.ok)
                    .map(|check| check.rule.clone())
                    .collect(),
            },
        };
        slides.push(standing);
    }
    // A stale page set: for another version of the deck, or for a deck that has since got shorter or
    // longer. Unchanged slides keep their standing; the count must still match.
    let stale = usable.is_none_or(|pages| {
        pages.deck.version_id != current_version_id || pages.deck.slide_count != deck.slides.len()
    });
    let deliverable = !slides.is_empty()
        && slides.iter().all(|slide| slide.state == SlideState::Ready)
        && usable.is_some_and(|pages| pages.deck.slide_count == deck.slides.len());
    Gate {
        deliverable,
        slides,
        stale,
    }
}

/// The base stylesheet the authority puts after the plane's: it fixes the page and cannot be
/// overridden by the template.
const BASE_CSS: &str = "html,body{margin:0!important;padding:0!important;background:#ffffff}\
@page{size:1280px 720px;margin:0}\
.slide{box-sizing:border-box!important;width:1280px!important;height:720px!important;overflow:hidden!important;\
position:relative!important;page-break-after:always;break-after:page}\
.slide:last-child{page-break-after:auto;break-after:auto}";

/// The content-security-policy of every page: no network, no script, no frame, inline styles and
/// inline images only.
pub const PAGE_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; \
font-src 'none'; script-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'";

/// The one trusted page shell: the plane's stylesheet, then the authority's, around a body.
#[must_use]
pub fn page_shell(css: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"{PAGE_CSP}\">\
<style>{css}</style><style>{BASE_CSS}</style></head><body>{body}</body></html>"
    )
}

/// What a fragment must be: one `section.slide` carrying `data-slide` equal to its index.
///
/// # Errors
/// A sentence naming the rule broken.
pub fn check_fragment(fragment: &str, index: usize) -> Result<(), html::Refusal> {
    let markup = html::check_markup(fragment)?;
    let [root] = markup.roots.as_slice() else {
        return Err(html::Refusal {
            rule: html::RULE_STRUCTURE,
            detail: format!(
                "a page is exactly one section element, not {}",
                markup.roots.len()
            ),
        });
    };
    let is_slide = root.tag == "section"
        && root
            .attrs
            .get("class")
            .is_some_and(|class| class.split_whitespace().any(|name| name == "slide"));
    if !is_slide {
        return Err(html::Refusal {
            rule: html::RULE_STRUCTURE,
            detail: "the page's root is not <section class=\"slide\">".to_string(),
        });
    }
    if root.attrs.get("data-slide").map(String::as_str) != Some(index.to_string().as_str()) {
        return Err(html::Refusal {
            rule: html::RULE_STRUCTURE,
            detail: format!("the page's data-slide is not {index}"),
        });
    }
    Ok(())
}

/// Collapse whitespace so text compares the way a reader reads it.
#[must_use]
pub fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A map of rule to the slides that fail it, for the version's record.
#[must_use]
pub fn failing_by_rule(entries: &[&PageEntry]) -> BTreeMap<String, Vec<usize>> {
    let mut out: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for entry in entries {
        for check in entry.checks.iter().filter(|check| !check.ok) {
            out.entry(check.rule.clone()).or_default().push(entry.index);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deck(slides: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"schema_version": "v1", "slides": slides})).expect("json")
    }

    fn parse(slides: Value) -> Result<PresentationSource, String> {
        parse_presentation(&deck(slides), 1 << 20)
    }

    const PIXEL: &str =
        "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

    #[test]
    fn slides_keep_a_closed_set_of_keys_and_each_layout_its_own() {
        let deck = parse(json!([
            {"title": "Plan", "body": "- one\n- two"},
            {"title": "Pic", "body": "x", "layout": "image", "image": PIXEL, "alt": "a pixel"},
            {"title": "Numbers", "body": "x", "layout": "chart", "alt": "bars",
             "chart": {"kind": "bar", "labels": ["a", "b"], "values": [1, 2.5], "unit": "k"}},
            {"title": "Split", "body": "left\n---\nright", "layout": "two_column", "notes": "say it"},
        ]))
        .expect("deck");
        assert_eq!(deck.slides.len(), 4);
        assert_eq!(deck.slides[0].layout, "content");
        assert_eq!(
            deck.slides[2].chart.as_ref().map(|c| c.values.clone()),
            Some(vec![1.0, 2.5])
        );
        assert_eq!(deck.slides[3].notes, "say it");
    }

    #[test]
    fn what_breaks_a_layout_or_the_closed_shape_is_refused() {
        let bad = |slide: Value| parse(json!([slide])).expect_err("refused");
        assert!(bad(json!({"title": "t", "body": "b", "surprise": 1})).contains("unknown keys"));
        assert!(bad(json!({"title": "t", "body": "b", "layout": "hero"})).contains("not a layout"));
        assert!(
            bad(json!({"title": "t", "body": "b", "layout": "image"})).contains("needs an image")
        );
        assert!(
            bad(json!({"title": "t", "body": "b", "layout": "image", "image": PIXEL}))
                .contains("alt")
        );
        assert!(bad(json!({"title": "t", "body": "b", "layout": "image", "alt": "x", "image": "https://x/y.png"})).contains("inline"));
        assert!(
            bad(json!({"title": "t", "body": "b", "image": PIXEL, "alt": "x"})).contains("belong")
        );
        assert!(
            bad(json!({"title": "t", "body": "b", "layout": "chart", "alt": "x"}))
                .contains("needs a chart")
        );
        assert!(bad(
            json!({"title": "t", "body": "b", "layout": "chart", "alt": "x",
            "chart": {"kind": "pie", "labels": ["a"], "values": [1]}})
        )
        .contains("bar or line"));
        assert!(bad(
            json!({"title": "t", "body": "b", "layout": "chart", "alt": "x",
            "chart": {"kind": "bar", "labels": ["a", "b"], "values": [1]}})
        )
        .contains("one label"));
        assert!(bad(
            json!({"title": "t", "body": "b", "layout": "chart", "alt": "x",
            "chart": {"kind": "bar", "labels": ["a"], "values": [1e-9]}})
        )
        .contains("magnitude"));
        assert!(bad(
            json!({"title": "t", "body": "b", "layout": "chart", "alt": "x",
            "chart": {"kind": "bar", "labels": ["a"], "values": [1], "extra": 1}})
        )
        .contains("unknown keys"));
        assert!(bad(json!({"title": " ", "body": "b"})).contains("needs a title"));
        assert!(bad(json!("nope")).contains("not an object"));
        assert!(parse_presentation(b"[]", 1000).is_err());
        assert!(parse_presentation(b"{\"schema_version\":\"v2\",\"slides\":[{}]}", 1000).is_err());
        assert!(parse_presentation(b"{\"schema_version\":\"v1\",\"slides\":[]}", 1000).is_err());
        let big = parse_presentation(
            &deck(
                json!([{"title": "t", "body": "b", "layout": "image", "alt": "x", "image": PIXEL}]),
            ),
            10,
        );
        assert!(big.expect_err("size").contains("over"));
    }

    #[test]
    fn a_slide_digest_follows_its_content_and_nothing_else() {
        let a = parse(json!([{"title": "Plan", "body": "x"}])).expect("a");
        let same_but_reordered_keys = parse(json!([{"body": "x", "title": "Plan"}])).expect("b");
        let changed = parse(json!([{"title": "Plan", "body": "y"}])).expect("c");
        assert_eq!(
            a.slides[0].digest(),
            same_but_reordered_keys.slides[0].digest()
        );
        assert_ne!(a.slides[0].digest(), changed.slides[0].digest());
        // Golden value: the plane computes the same digest (python/tests/intelligence/test_deck_pages.py).
        assert_eq!(
            sha256_hex("{\"body\":\"x\",\"title\":\"Plan\"}"),
            a.slides[0].digest()
        );
        // Non-ASCII stays unescaped.
        assert_eq!(canonical(&json!({"t": "é—"})), "{\"t\":\"é—\"}");
    }

    #[test]
    fn body_lines_are_what_a_page_must_say() {
        assert_eq!(
            body_lines("Intro line\n\n- first\n-  second \n---\n— Ada Lovelace\n   \nplain - dash"),
            vec![
                "Intro line",
                "first",
                "second",
                "Ada Lovelace",
                "plain - dash"
            ]
        );
        assert!(body_lines("---\n\n").is_empty());
    }

    fn pages_for(deck: &PresentationSource, version: &str, states: &[SlideState]) -> DeckPages {
        DeckPages {
            schema_version: "v1".to_string(),
            template_version: "v1".to_string(),
            deck: DeckRef {
                artifact_id: "art_X".to_string(),
                version_id: version.to_string(),
                content_digest: "0".repeat(64),
                slide_count: deck.slides.len(),
            },
            css: String::new(),
            slides: deck
                .slides
                .iter()
                .zip(states)
                .enumerate()
                .map(|(index, (slide, state))| PageEntry {
                    index,
                    source_digest: slide.digest(),
                    state: *state,
                    fragment: matches!(state, SlideState::Ready | SlideState::FailedQuality).then(
                        || format!("<section class=\"slide\" data-slide=\"{index}\"></section>"),
                    ),
                    page_digest: matches!(state, SlideState::Ready | SlideState::FailedQuality)
                        .then(|| "1".repeat(64)),
                    checks: if *state == SlideState::FailedQuality {
                        vec![PageCheck {
                            rule: "output.deck.page.overflow".to_string(),
                            ok: false,
                            detail: "clipped".to_string(),
                        }]
                    } else {
                        Vec::new()
                    },
                    text_digest: None,
                    evidence_ids: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_deck_is_deliverable_only_when_every_page_of_the_current_version_is_ready() {
        let deck =
            parse(json!([{"title": "A", "body": "a"}, {"title": "B", "body": "b"}])).expect("deck");
        let ready = pages_for(&deck, "artv_1", &[SlideState::Ready, SlideState::Ready]);
        let verdict = gate(&deck, "artv_1", Some(&ready), "v1");
        assert!(verdict.deliverable && !verdict.stale);
        assert!(verdict.blockers().is_empty());

        let failing = pages_for(
            &deck,
            "artv_1",
            &[SlideState::Ready, SlideState::FailedQuality],
        );
        let verdict = gate(&deck, "artv_1", Some(&failing), "v1");
        assert!(!verdict.deliverable);
        assert_eq!(verdict.blockers().len(), 1);
        assert_eq!(
            verdict.blockers()[0].failed_rules,
            vec!["output.deck.page.overflow"]
        );

        let pending = pages_for(&deck, "artv_1", &[SlideState::Ready, SlideState::Pending]);
        assert!(!gate(&deck, "artv_1", Some(&pending), "v1").deliverable);

        assert!(!gate(&deck, "artv_1", None, "v1").deliverable);
        let none = gate(&deck, "artv_1", None, "v1");
        assert!(none
            .slides
            .iter()
            .all(|slide| slide.state == SlideState::Pending));
    }

    #[test]
    fn an_edited_slide_is_edited_whatever_the_stored_state_says() {
        let before = parse(json!([{"title": "A", "body": "a"}, {"title": "B", "body": "b"}]))
            .expect("before");
        let ready = pages_for(&before, "artv_1", &[SlideState::Ready, SlideState::Ready]);
        // The deck got a new version in which only slide 1 changed.
        let after = parse(json!([{"title": "A", "body": "a"}, {"title": "B", "body": "b2"}]))
            .expect("after");
        let verdict = gate(&after, "artv_2", Some(&ready), "v1");
        assert!(!verdict.deliverable);
        assert!(verdict.stale);
        assert_eq!(
            verdict.slides[0].state,
            SlideState::Ready,
            "an unchanged slide keeps its page"
        );
        assert_eq!(verdict.slides[1].state, SlideState::Edited);
        // A slide added at the end is pending, and a deck that only got longer is not deliverable.
        let longer = parse(json!([{"title": "A", "body": "a"}, {"title": "B", "body": "b"}, {"title": "C", "body": "c"}])).expect("longer");
        let verdict = gate(&longer, "artv_3", Some(&ready), "v1");
        assert_eq!(verdict.slides[2].state, SlideState::Pending);
        assert!(!verdict.deliverable);
        // A shorter deck whose slides all match is still not deliverable from pages of another length.
        let shorter = parse(json!([{"title": "A", "body": "a"}])).expect("shorter");
        let verdict = gate(&shorter, "artv_4", Some(&ready), "v1");
        assert!(
            !verdict.deliverable,
            "the pages are for a deck of two slides"
        );
        // A deck re-saved with identical slides keeps its pages.
        let same = gate(&before, "artv_9", Some(&ready), "v1");
        assert!(same.deliverable && same.stale);
    }

    #[test]
    fn pages_from_another_template_version_are_not_trusted() {
        let deck = parse(json!([{"title": "A", "body": "a"}])).expect("deck");
        let mut ready = pages_for(&deck, "artv_1", &[SlideState::Ready]);
        ready.template_version = "v0".to_string();
        assert!(!gate(&deck, "artv_1", Some(&ready), "v1").deliverable);
    }

    #[test]
    fn stored_pages_are_shape_checked_so_a_hand_made_manifest_cannot_pass_as_ready() {
        let deck = parse(json!([{"title": "A", "body": "a"}])).expect("deck");
        let good = pages_for(&deck, "artv_1", &[SlideState::Ready]);
        assert!(DeckPages::parse(&good.to_bytes()).is_ok());
        let mut no_page = good.clone();
        no_page.slides[0].fragment = None;
        assert!(DeckPages::parse(&no_page.to_bytes())
            .expect_err("page")
            .contains("no page"));
        let mut lying = good.clone();
        lying.slides[0].checks.push(PageCheck {
            rule: "r".into(),
            ok: false,
            detail: "d".into(),
        });
        assert!(DeckPages::parse(&lying.to_bytes())
            .expect_err("ready")
            .contains("failing check"));
        let mut hollow = pages_for(&deck, "artv_1", &[SlideState::FailedQuality]);
        hollow.slides[0].checks.clear();
        assert!(DeckPages::parse(&hollow.to_bytes())
            .expect_err("failed")
            .contains("no failing check"));
        let mut miscounted = good.clone();
        miscounted.deck.slide_count = 3;
        assert!(DeckPages::parse(&miscounted.to_bytes()).is_err());
        assert!(DeckPages::parse(b"{\"nope\":1}").is_err());
        let mut extra: Value = serde_json::from_slice(&good.to_bytes()).expect("json");
        extra["slides"][0]["surprise"] = json!(1);
        assert!(DeckPages::parse(&serde_json::to_vec(&extra).expect("bytes")).is_err());
    }

    #[test]
    fn a_fragment_is_exactly_one_slide_section_for_its_index() {
        assert!(check_fragment(
            "<section class=\"slide a\" data-slide=\"3\"><p>x</p></section>",
            3
        )
        .is_ok());
        for (fragment, why) in [
            ("<section class=\"slide\" data-slide=\"2\"></section>", "wrong index"),
            ("<section class=\"slide\"></section>", "no index"),
            ("<div class=\"slide\" data-slide=\"3\"></div>", "not a section"),
            ("<section class=\"other\" data-slide=\"3\"></section>", "not a slide"),
            ("<section class=\"slide\" data-slide=\"3\"></section><section class=\"slide\" data-slide=\"3\"></section>", "two"),
            ("<p>x</p>", "no section"),
            ("<section class=\"slide\" data-slide=\"3\"><script>x</script></section>", "script"),
            ("", "empty"),
        ] {
            assert!(check_fragment(fragment, 3).is_err(), "{why}");
        }
    }

    #[test]
    fn the_shell_fixes_the_page_after_the_planes_stylesheet_and_forbids_everything_else() {
        let page = page_shell(
            ".slide{width:10px;overflow:visible}",
            "<section class=\"slide\" data-slide=\"0\"></section>",
        );
        let plane = page.find(".slide{width:10px").expect("plane css");
        let base = page.find("width:1280px!important").expect("base css");
        assert!(
            plane < base,
            "the authority's rules come last and are !important"
        );
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("script-src 'none'"));
        assert!(page.starts_with("<!doctype html>"));
    }
}
