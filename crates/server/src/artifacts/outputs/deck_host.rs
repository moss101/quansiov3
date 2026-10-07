//! `deck.render`, `deck.export` and `deck.deliver` (ADP-006, DOMAIN.md §10.4 B).
//!
//! The plane hands over the markup it built; the authority lays each page out in the render sandbox,
//! measures it, and records the result per slide in the tool-owned deck pages. Delivery and export
//! read those pages back through the gate in [`super::deck`]: nothing is delivered on the word of the
//! proposer, and nothing hand-made passes as the authority's own.

use std::collections::BTreeSet;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use futures_util::stream::{self, StreamExt};
use quansio_core::Digest;
use serde_json::{json, Value};

use super::deck::{
    self, DeckPages, DeckRef, Gate, PageCheck, PageEntry, PresentationSource, SlideState,
    PAGES_MEDIA_TYPE, PAGES_ROLE, PDF_MEDIA_TYPE, PDF_ROLE, PPTX_MEDIA_TYPE, PPTX_ROLE,
};
use super::ooxml::{self, PackageLimits, SlideExpect};
use super::pdf;
use super::ports::{MeasureSpec, PdfSpec, PortError, RenderJob, RenderResult};
use super::quality::{Check, QualityRecord, Trust};
use super::support::{
    artifact_uri, commit_output, find_output, read_source, read_version_bytes, record_screenshot,
    refuse, refuse_with, success, unavailable, Ctx, Loaded, OutputSpec,
};
use crate::artifacts::{ArtifactKind, ArtifactVersionId};
use crate::runtime::turn_loop::{HostFailure, HostOutcome};

const RENDER: &str = "deck.render";
const EXPORT: &str = "deck.export";

fn pages_role(deck_id: &str) -> String {
    format!("{PAGES_ROLE}:{deck_id}")
}

fn parse_deck(ctx: &Ctx<'_>, loaded: &Loaded) -> Result<PresentationSource, HostOutcome> {
    deck::parse_presentation(&loaded.bytes, ctx.env.config.deck.image_max_bytes).map_err(|detail| {
        refuse(
            "VALIDATION_SCHEMA",
            "artifact.format",
            format!("the deck's current version is not a valid presentation: {detail}"),
        )
    })
}

/// The pages the authority itself wrote for the deck's current state, if there are any: a pages
/// Artifact whose current version carries a quality record by `deck.render`. An upload with the
/// right media type has no record and is not these pages.
async fn authentic_pages(
    ctx: &Ctx<'_>,
    deck_id: &str,
) -> Result<Option<(DeckPages, Committed2)>, HostOutcome> {
    let Some((artifact, version)) = find_output(ctx, &pages_role(deck_id)).await? else {
        return Ok(None);
    };
    let record = version.quality_record.as_ref();
    let by_the_authority = version.media_type == PAGES_MEDIA_TYPE
        && record
            .and_then(|record| record.get("adapter"))
            .and_then(Value::as_str)
            == Some(RENDER)
        && record
            .and_then(|record| record.get("record_version"))
            .and_then(Value::as_u64)
            == Some(1);
    if !by_the_authority {
        return Ok(None);
    }
    let bytes = read_version_bytes(ctx, &version.id).await?;
    let Ok(pages) = DeckPages::parse(&bytes) else {
        return Ok(None);
    };
    if pages.deck.artifact_id != deck_id {
        return Ok(None);
    }
    Ok(Some((
        pages,
        Committed2 {
            artifact_id: artifact.id.to_canonical(),
            version_id: version.id.clone(),
        },
    )))
}

/// Where the authentic pages are stored.
struct Committed2 {
    artifact_id: String,
    version_id: ArtifactVersionId,
}

fn standings_json(gate: &Gate) -> Value {
    Value::Array(
        gate.slides
            .iter()
            .map(|slide| {
                json!({
                    "index": slide.index,
                    "state": slide.state.as_str(),
                    "failed_rules": slide.failed_rules,
                })
            })
            .collect(),
    )
}

fn not_deliverable(gate: &Gate) -> HostOutcome {
    let pages: Vec<Value> = gate
        .blockers()
        .into_iter()
        .map(|slide| {
            json!({
                "index": slide.index,
                "state": slide.state.as_str(),
                "failed_rules": slide.failed_rules,
            })
        })
        .collect();
    let message = format!(
        "the deck is not deliverable: {} of {} pages are not ready ({})",
        pages.len(),
        gate.slides.len(),
        gate.blockers()
            .iter()
            .take(6)
            .map(|slide| format!("slide {} is {}", slide.index, slide.state.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    refuse_with(
        "DECK_NOT_DELIVERABLE",
        "output.deck.gate",
        message,
        Some(json!({ "pages": pages, "stale": gate.stale })),
    )
}

// ------------------------------------------------------------------------------------------------
// deck.render
// ------------------------------------------------------------------------------------------------

/// Judge one laid-out page against its slide: the checks and the digest of the text the browser read.
fn judge_page(
    slide: &deck::SlideSource,
    index: usize,
    result: &RenderResult,
    viewport: (u32, u32),
) -> (Vec<PageCheck>, String) {
    let report = &result.measurements;
    let roots = report.get("slide_roots").and_then(Value::as_u64);
    let page = report.pointer("/slides/0");
    let text = page
        .and_then(|page| page.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let flat_text = deck::flat(text);
    let mut checks = Vec::new();
    let mut push = |rule: &str, ok: bool, held: String, broke: String| {
        checks.push(PageCheck {
            rule: format!("output.deck.page.{rule}"),
            ok,
            detail: if ok { held } else { broke },
        });
    };

    let shaped = roots == Some(1)
        && page.is_some_and(|page| {
            page.get("index").and_then(Value::as_str) == Some(index.to_string().as_str())
                && page.get("width").and_then(Value::as_u64) == Some(u64::from(viewport.0))
                && page.get("height").and_then(Value::as_u64) == Some(u64::from(viewport.1))
        });
    push(
        "shape",
        shaped,
        format!("one {}x{} slide root", viewport.0, viewport.1),
        format!(
            "the browser found {} slide roots or one of another size or index",
            roots.map_or_else(|| "no".to_string(), |n| n.to_string())
        ),
    );

    let overflow = page
        .and_then(|page| page.pointer("/overflow/count"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let first = page
        .and_then(|page| page.pointer("/overflow/first"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    push(
        "overflow",
        overflow == 0,
        "content fits the page".to_string(),
        format!(
            "{overflow} element(s) are clipped or painted outside the page, starting at {first:?}"
        ),
    );

    let chars = page
        .and_then(|page| page.get("text_chars"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let figures = page
        .and_then(|page| page.get("figures"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    push(
        "empty",
        chars > 0 || figures > 0,
        format!("{chars} characters of text and {figures} figures"),
        "the page shows no text and no figure".to_string(),
    );

    let missing: usize = page
        .and_then(|page| page.get("missing_alt"))
        .and_then(Value::as_array)
        .map_or(1, Vec::len);
    push(
        "alt_text",
        missing == 0,
        "every image and figure has an accessible name".to_string(),
        format!("{missing} image or figure has no alt text"),
    );

    let failures = page
        .and_then(|page| page.pointer("/contrast/failures"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let unverifiable = page
        .and_then(|page| page.pointer("/contrast/unverifiable"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let checked = page
        .and_then(|page| page.pointer("/contrast/checked"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    push(
        "contrast",
        page.is_some() && failures.is_empty() && unverifiable == 0,
        format!("{checked} text elements meet the contrast thresholds"),
        if failures.is_empty() {
            format!("{unverifiable} text element(s) sit on a background that cannot be established from style")
        } else {
            let sample: Vec<String> = failures
                .iter()
                .take(3)
                .map(|failure| {
                    format!(
                        "{:?} at {} (needs {})",
                        failure
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        failure
                            .get("ratio")
                            .map_or_else(String::new, ToString::to_string),
                        failure
                            .get("needed")
                            .map_or_else(String::new, ToString::to_string)
                    )
                })
                .collect();
            format!(
                "{} text element(s) are below the contrast threshold: {}",
                failures.len(),
                sample.join("; ")
            )
        },
    );

    let mut absent: Vec<String> = Vec::new();
    if !flat_text.contains(&deck::flat(&slide.title)) {
        absent.push(format!("the title {:?}", slide.title));
    }
    for line in slide.lines() {
        if !flat_text.contains(&deck::flat(&line)) {
            absent.push(format!(
                "the line {:?}",
                line.chars().take(40).collect::<String>()
            ));
        }
    }
    push(
        "source_text",
        absent.is_empty(),
        "the page says the slide's title and every body line".to_string(),
        format!("the page does not say {}", absent.join(", ")),
    );

    push(
        "network",
        result.blocked.is_empty(),
        "the page asked for nothing outside itself".to_string(),
        format!(
            "the page asked for {} thing(s) it was refused: {}",
            result.blocked.len(),
            result
                .blocked
                .iter()
                .take(3)
                .map(|blocked| format!("{} {}", blocked.scheme, blocked.resource_type))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    (checks, deck::sha256_hex(&flat_text))
}

fn render_failure(error: PortError) -> HostOutcome {
    match error {
        PortError::Unavailable { adapter, detail } => unavailable(adapter, detail, None),
        PortError::Failed(detail) => HostOutcome::Failure {
            retryable: true,
            error: json!({ "code": "INTERNAL", "rule": "output.render", "message": detail }),
            evidence_ids: Vec::new(),
        },
        PortError::TooLarge(detail) => refuse("VALIDATION_BOUNDS", "output.render.size", detail),
    }
}

struct Supplied {
    index: usize,
    fragment: String,
}

fn parse_pages(ctx: &Ctx<'_>, slide_count: usize) -> Result<(String, Vec<Supplied>), HostOutcome> {
    let config = &ctx.env.config.deck;
    let css = ctx.arg_str("css").unwrap_or_default().to_string();
    if css.len() > config.css_max_bytes {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.deck.css",
            format!(
                "the stylesheet is {} bytes; at most {}",
                css.len(),
                config.css_max_bytes
            ),
        ));
    }
    super::html::check_css(&css).map_err(|refusal| {
        refuse(
            "VALIDATION_SCHEMA",
            refusal.rule,
            format!("the stylesheet: {}", refusal.detail),
        )
    })?;
    let pages = ctx
        .dispatch
        .args
        .get("pages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            refuse(
                "VALIDATION_SCHEMA",
                "output.deck.pages",
                "pages must be a list",
            )
        })?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for page in pages {
        let index = page
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .ok_or_else(|| {
                refuse(
                    "VALIDATION_SCHEMA",
                    "output.deck.pages",
                    "a page needs an index",
                )
            })?;
        if index >= slide_count {
            return Err(refuse(
                "VALIDATION_BOUNDS",
                "output.deck.pages",
                format!("page {index} is beyond the deck's {slide_count} slides"),
            ));
        }
        if !seen.insert(index) {
            return Err(refuse(
                "VALIDATION_SCHEMA",
                "output.deck.pages",
                format!("page {index} is given twice"),
            ));
        }
        let fragment = page
            .get("fragment")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if fragment.len() > config.page_max_bytes {
            return Err(refuse(
                "VALIDATION_BOUNDS",
                "output.deck.page.size",
                format!(
                    "page {index} is {} bytes; at most {}",
                    fragment.len(),
                    config.page_max_bytes
                ),
            ));
        }
        deck::check_fragment(&fragment, index).map_err(|refusal| {
            refuse(
                "VALIDATION_SCHEMA",
                refusal.rule,
                format!("page {index}: {}", refusal.detail),
            )
        })?;
        out.push(Supplied { index, fragment });
    }
    out.sort_by_key(|page| page.index);
    Ok((css, out))
}

/// `deck.render`.
pub async fn render(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(render_inner(ctx).await.unwrap_or_else(|outcome| outcome))
}

async fn render_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let config = &ctx.env.config;
    let deck_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let loaded = read_source(ctx, &deck_id, ArtifactKind::Presentation, None).await?;
    let source = parse_deck(ctx, &loaded)?;
    if source.slides.len() > config.deck.max_slides {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.deck.slides",
            format!("a deck has at most {} slides", config.deck.max_slides),
        ));
    }
    let (css, supplied) = parse_pages(ctx, source.slides.len())?;
    let Some(port) = ctx.env.render.clone() else {
        return Err(unavailable(
            "render",
            "no render host is composed on this deployment, so no page can be laid out or measured",
            None,
        ));
    };

    // Which entries carry over from the pages already recorded.
    let previous = authentic_pages(ctx, &deck_id)
        .await?
        .map(|(pages, _)| pages);
    let carried = previous
        .as_ref()
        .filter(|pages| pages.template_version == config.deck.template_version && pages.css == css);
    let digests: Vec<String> = source
        .slides
        .iter()
        .map(deck::SlideSource::digest)
        .collect();
    let mut entries: Vec<Option<PageEntry>> = Vec::with_capacity(digests.len());
    for (index, digest) in digests.iter().enumerate() {
        let entry = if supplied.iter().any(|page| page.index == index) {
            None
        } else if let Some(entry) =
            carried
                .and_then(|pages| pages.slides.get(index))
                .filter(|entry| {
                    entry.source_digest == *digest
                        && matches!(entry.state, SlideState::Ready | SlideState::FailedQuality)
                })
        {
            Some(entry.clone())
        } else {
            let known = previous
                .as_ref()
                .is_some_and(|pages| index < pages.slides.len());
            Some(PageEntry {
                index,
                source_digest: digest.clone(),
                state: if known {
                    SlideState::Edited
                } else {
                    SlideState::Pending
                },
                fragment: None,
                page_digest: None,
                checks: Vec::new(),
                text_digest: None,
                evidence_ids: Vec::new(),
            })
        };
        entries.push(entry);
    }

    // Lay the supplied pages out, a few at a time.
    let measure = MeasureSpec::Slides {
        overflow_tolerance_px: config.deck.overflow_tolerance_px,
        min_contrast_text: config.deck.min_contrast_text,
        min_contrast_large: config.deck.min_contrast_large,
    };
    let viewport = (config.render.viewport.width, config.render.viewport.height);
    let jobs: Vec<(String, RenderJob)> = supplied
        .iter()
        .map(|page| {
            let job = RenderJob {
                html: deck::page_shell(&css, &page.fragment),
                assets: std::collections::BTreeMap::new(),
                viewport,
                measure,
                pdf: None,
                screenshot: true,
                max_screenshot_bytes: config.render.screenshot_max_bytes,
                deadline_ms: config.render.deadline_ms,
            };
            (job.html.clone(), job)
        })
        .collect();
    let results: Vec<(String, Result<RenderResult, PortError>)> =
        stream::iter(jobs.into_iter().map(|(html, job)| {
            let port = port.clone();
            async move { (html, port.render(job).await) }
        }))
        .buffered(3)
        .collect()
        .await;

    let mut browser = String::new();
    let mut evidence_ids = Vec::new();
    let mut rendered: Vec<usize> = Vec::new();
    for (page, (html, result)) in supplied.iter().zip(results) {
        let result = result.map_err(render_failure)?;
        if browser.is_empty() {
            browser.clone_from(&result.browser);
        }
        let slide = &source.slides[page.index];
        let (checks, text_digest) = judge_page(slide, page.index, &result, viewport);
        let ready = checks.iter().all(|check| check.ok);
        let mut shots = Vec::new();
        if let Some(png) = result.screenshot {
            let id = record_screenshot(
                ctx,
                png,
                format!(
                    "slide {} of {deck_id} as the browser laid it out: {}",
                    page.index,
                    if ready { "ready" } else { "failed quality" }
                ),
                RENDER,
            )
            .await?;
            evidence_ids.push(id.clone());
            shots.push(id);
        }
        entries[page.index] = Some(PageEntry {
            index: page.index,
            source_digest: digests[page.index].clone(),
            state: if ready {
                SlideState::Ready
            } else {
                SlideState::FailedQuality
            },
            fragment: Some(page.fragment.clone()),
            page_digest: Some(deck::sha256_hex(&html)),
            checks,
            text_digest: Some(text_digest),
            evidence_ids: shots,
        });
        rendered.push(page.index);
    }
    let slides: Vec<PageEntry> = entries.into_iter().flatten().collect();

    let pages = DeckPages {
        schema_version: "v1".to_string(),
        template_version: config.deck.template_version.clone(),
        deck: DeckRef {
            artifact_id: deck_id.clone(),
            version_id: loaded.version.id.to_canonical(),
            content_digest: loaded.version.content_digest.to_string(),
            slide_count: slides.len(),
        },
        css,
        slides,
    };
    let all_ready = pages
        .slides
        .iter()
        .all(|entry| entry.state == SlideState::Ready);

    // The record: one check per rule over the pages this call rendered, and whether the deck is whole.
    let mut record = QualityRecord::new(RENDER, Trust::UntrustedExternal);
    record.inputs.push(loaded.input());
    record
        .environment
        .insert("browser".to_string(), Value::String(browser));
    record.evidence_ids.clone_from(&evidence_ids);
    let rendered_entries: Vec<&PageEntry> =
        rendered.iter().map(|index| &pages.slides[*index]).collect();
    let failing = deck::failing_by_rule(&rendered_entries);
    for rule in [
        "shape",
        "overflow",
        "empty",
        "alt_text",
        "contrast",
        "source_text",
        "network",
    ] {
        let rule = format!("output.deck.page.{rule}");
        match failing.get(&rule) {
            None => record.checks.push(Check::passed(
                &rule,
                format!("held on all {} rendered page(s)", rendered_entries.len()),
            )),
            Some(slides) => record.checks.push(Check::failed(
                &rule,
                format!(
                    "failed on slide(s) {}",
                    slides
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }
    let not_ready: Vec<String> = pages
        .slides
        .iter()
        .filter(|entry| entry.state != SlideState::Ready)
        .map(|entry| format!("{} is {}", entry.index, entry.state.as_str()))
        .collect();
    record.checks.push(if not_ready.is_empty() {
        Check::passed(
            "output.deck.pages.ready",
            format!("all {} pages are ready", pages.slides.len()),
        )
    } else {
        Check::failed(
            "output.deck.pages.ready",
            format!("slides not ready: {}", not_ready.join(", ")),
        )
    });

    let bytes = pages.to_bytes();
    let committed = commit_output(
        ctx,
        OutputSpec {
            role: pages_role(&deck_id),
            kind: ArtifactKind::Other,
            title: format!("Pages of {}", loaded.artifact.title),
            media_type: PAGES_MEDIA_TYPE.to_string(),
            bytes,
            record,
        },
    )
    .await?;

    let gate = deck::gate(
        &source,
        &loaded.version.id.to_canonical(),
        Some(&pages),
        &config.deck.template_version,
    );
    let version_id = committed.version.id.to_canonical();
    Ok(success(
        json!({
            "artifact_id": committed.artifact_id.to_canonical(),
            "version_id": version_id,
            "version": committed.version.seq,
            "deck_version_id": loaded.version.id.to_canonical(),
            "deliverable": all_ready && gate.deliverable,
            "slides": standings_json(&gate),
            "evidence_ids": evidence_ids,
            "trust": Trust::UntrustedExternal.as_str(),
        }),
        evidence_ids,
        Some(version_id),
    ))
}

// ------------------------------------------------------------------------------------------------
// deck.deliver
// ------------------------------------------------------------------------------------------------

async fn gate_for(
    ctx: &Ctx<'_>,
    deck_id: &str,
) -> Result<
    (
        Loaded,
        PresentationSource,
        Option<(DeckPages, Committed2)>,
        Gate,
    ),
    HostOutcome,
> {
    let loaded = read_source(ctx, deck_id, ArtifactKind::Presentation, None).await?;
    let source = parse_deck(ctx, &loaded)?;
    let pages = authentic_pages(ctx, deck_id).await?;
    let verdict = deck::gate(
        &source,
        &loaded.version.id.to_canonical(),
        pages.as_ref().map(|(pages, _)| pages),
        &ctx.env.config.deck.template_version,
    );
    Ok((loaded, source, pages, verdict))
}

/// `deck.deliver`: the completion gate.
pub async fn deliver(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(deliver_inner(ctx).await.unwrap_or_else(|outcome| outcome))
}

async fn deliver_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let deck_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let (loaded, source, _pages, verdict) = gate_for(ctx, &deck_id).await?;
    if !verdict.deliverable {
        return Err(not_deliverable(&verdict));
    }
    let mut exports = Vec::new();
    for (format, role) in [("pdf", PDF_ROLE), ("pptx", PPTX_ROLE)] {
        if let Some((artifact, version)) = find_output(ctx, &format!("{role}:{deck_id}")).await? {
            let record = version.quality_record.as_ref();
            let current = record
                .and_then(|record| record.pointer("/inputs/0/content_digest"))
                .and_then(Value::as_str)
                == Some(loaded.version.content_digest.as_str());
            let ok = record
                .and_then(|record| record.get("ok"))
                .and_then(Value::as_bool)
                == Some(true);
            if current && ok {
                exports.push(json!({
                    "format": format,
                    "artifact_id": artifact.id.to_canonical(),
                    "version_id": version.id.to_canonical(),
                    "uri": artifact_uri(&artifact.id.to_canonical()),
                }));
            }
        }
    }
    Ok(success(
        json!({
            "uri": artifact_uri(&deck_id),
            "artifact_id": deck_id,
            "version_id": loaded.version.id.to_canonical(),
            "content_digest": loaded.version.content_digest.to_string(),
            "slide_count": source.slides.len(),
            "exports": exports,
        }),
        Vec::new(),
        Some(loaded.version.id.to_canonical()),
    ))
}

// ------------------------------------------------------------------------------------------------
// deck.export
// ------------------------------------------------------------------------------------------------

/// `deck.export`.
pub async fn export(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(export_inner(ctx).await.unwrap_or_else(|outcome| outcome))
}

async fn export_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let deck_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let format = ctx.arg_str("format").unwrap_or_default().to_string();
    let (loaded, source, pages, verdict) = gate_for(ctx, &deck_id).await?;
    if !verdict.deliverable {
        return Err(not_deliverable(&verdict));
    }
    let Some((pages, pages_at)) = pages else {
        return Err(not_deliverable(&verdict));
    };
    let mut record = QualityRecord::new(EXPORT, Trust::AgentGenerated);
    record.inputs.push(loaded.input());
    record.inputs.push(super::quality::Input {
        artifact_id: pages_at.artifact_id.clone(),
        version_id: pages_at.version_id.to_canonical(),
        content_digest: pages.deck.content_digest.clone(),
    });

    let (role, media_type, bytes, title, pages_out) = match format.as_str() {
        "pdf" => {
            let (bytes, checks, browser, count) = print_deck(ctx, &pages).await?;
            record.checks = checks;
            record
                .environment
                .insert("browser".to_string(), Value::String(browser));
            (
                PDF_ROLE,
                PDF_MEDIA_TYPE,
                bytes,
                format!("{} (PDF)", loaded.artifact.title),
                Some(count),
            )
        }
        "pptx" => {
            let (bytes, checks) = take_pptx(ctx, &source)?;
            record.checks = checks;
            (
                PPTX_ROLE,
                PPTX_MEDIA_TYPE,
                bytes,
                format!("{} (PPTX)", loaded.artifact.title),
                None,
            )
        }
        other => {
            return Err(refuse(
                "VALIDATION_SCHEMA",
                "output.deck.export.format",
                format!("{other:?} is not an export format"),
            ))
        }
    };
    if let Some(failure) = record.first_failure() {
        return Err(refuse_with(
            "VALIDATION_SCHEMA",
            record
                .failures()
                .first()
                .map_or("output.deck.export", |check| check.rule.as_str()),
            format!("the {format} export failed its checks, so no version was made: {failure}"),
            Some(json!({ "checks": record.checks.iter().map(Check::to_json).collect::<Vec<_>>() })),
        ));
    }
    let size = bytes.len();
    let committed = commit_output(
        ctx,
        OutputSpec {
            role: format!("{role}:{deck_id}"),
            kind: ArtifactKind::Other,
            title,
            media_type: media_type.to_string(),
            bytes,
            record,
        },
    )
    .await?;
    let version_id = committed.version.id.to_canonical();
    let mut output = json!({
        "artifact_id": committed.artifact_id.to_canonical(),
        "version_id": version_id,
        "version": committed.version.seq,
        "format": format,
        "content_digest": committed.version.content_digest.to_string(),
        "size_bytes": size,
        "replayed": committed.replayed,
        "trust": Trust::AgentGenerated.as_str(),
    });
    if let Some(count) = pages_out {
        output["pages"] = json!(count);
    }
    Ok(success(output, Vec::new(), Some(version_id)))
}

/// Print every stored page into one PDF, one PDF page per slide.
async fn print_deck(
    ctx: &Ctx<'_>,
    pages: &DeckPages,
) -> Result<(Vec<u8>, Vec<Check>, String, usize), HostOutcome> {
    let config = &ctx.env.config;
    let Some(port) = ctx.env.render.clone() else {
        return Err(unavailable(
            "render",
            "no render host is composed on this deployment, so nothing can be printed",
            None,
        ));
    };
    let body: String = pages
        .slides
        .iter()
        .map(|entry| entry.fragment.clone().unwrap_or_default())
        .collect();
    let result = port
        .render(RenderJob {
            html: deck::page_shell(&pages.css, &body),
            assets: std::collections::BTreeMap::new(),
            viewport: (config.render.viewport.width, config.render.viewport.height),
            measure: MeasureSpec::Slides {
                overflow_tolerance_px: config.deck.overflow_tolerance_px,
                min_contrast_text: config.deck.min_contrast_text,
                min_contrast_large: config.deck.min_contrast_large,
            },
            pdf: Some(PdfSpec {
                paper_width_in: config.deck.pdf.paper_width_in,
                paper_height_in: config.deck.pdf.paper_height_in,
                margin_in: 0.0,
                prefer_css_page_size: true,
                max_bytes: config.deck.pdf.max_bytes,
            }),
            screenshot: false,
            max_screenshot_bytes: config.render.screenshot_max_bytes,
            deadline_ms: config.render.deadline_ms,
        })
        .await
        .map_err(render_failure)?;
    let Some(bytes) = result.pdf else {
        return Err(refuse(
            "INTERNAL",
            "output.deck.pdf",
            "the render produced no PDF",
        ));
    };
    let slide_roots = result
        .measurements
        .get("slide_roots")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut checks = Vec::new();
    match pdf::inspect(&bytes) {
        Ok(info) => {
            checks.push(Check::passed(
                "output.deck.pdf.structure",
                format!(
                    "a PDF {} with a catalog and a page tree, no active content",
                    info.version
                ),
            ));
            checks.push(Check::of(
                "output.deck.pdf.pages",
                info.pages == pages.slides.len(),
                format!("{} PDF pages for {} slides", info.pages, pages.slides.len()),
            ));
        }
        Err(detail) => {
            checks.push(Check::failed("output.deck.pdf.structure", detail));
            checks.push(Check::failed(
                "output.deck.pdf.pages",
                "not checked: the PDF does not parse",
            ));
        }
    }
    checks.push(Check::of(
        "output.deck.pdf.slides",
        usize::try_from(slide_roots).ok() == Some(pages.slides.len()),
        format!(
            "the browser printed {slide_roots} slide pages of {}",
            pages.slides.len()
        ),
    ));
    checks.push(Check::of(
        "output.deck.pdf.network",
        result.blocked.is_empty(),
        if result.blocked.is_empty() {
            "the print asked for nothing outside itself".to_string()
        } else {
            format!(
                "the print asked for {} thing(s) it was refused",
                result.blocked.len()
            )
        },
    ));
    let count = pdf::inspect(&bytes).map_or(0, |info| info.pages);
    Ok((bytes, checks, result.browser, count))
}

/// Take the package a call carries and verify it against the deck.
fn take_pptx(
    ctx: &Ctx<'_>,
    source: &PresentationSource,
) -> Result<(Vec<u8>, Vec<Check>), HostOutcome> {
    let limits = &ctx.env.config.deck.pptx;
    let (Some(encoded), Some(declared)) =
        (ctx.arg_str("content_base64"), ctx.arg_str("content_digest"))
    else {
        return Err(refuse(
            "VALIDATION_SCHEMA",
            "output.deck.pptx.package",
            "a pptx export carries the package as content_base64 with its content_digest",
        ));
    };
    let bytes = STANDARD.decode(encoded.trim()).map_err(|error| {
        refuse(
            "VALIDATION_SCHEMA",
            "output.deck.pptx.package",
            format!("content_base64: {error}"),
        )
    })?;
    if bytes.len() > limits.max_bytes {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.deck.pptx.package",
            format!(
                "the package is {} bytes; at most {}",
                bytes.len(),
                limits.max_bytes
            ),
        ));
    }
    let actual = Digest::of(&bytes);
    if declared != actual.as_str() {
        return Err(refuse(
            "VALIDATION_SCHEMA",
            "artifact.content_digest",
            format!("content_digest {declared:?} is not the digest of the package ({actual})"),
        ));
    }
    let expect: Vec<SlideExpect> = source
        .slides
        .iter()
        .map(|slide| SlideExpect {
            title: slide.title.clone(),
            lines: slide.lines(),
            notes: slide.notes.clone(),
        })
        .collect();
    let checks = ooxml::verify_pptx(
        &bytes,
        &expect,
        PackageLimits {
            max_parts: limits.max_parts,
            max_part_bytes: limits.max_part_bytes,
            max_bytes: limits.max_bytes,
        },
    );
    Ok((bytes, checks))
}
