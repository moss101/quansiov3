//! `document.export_pdf` and `diagram.render` (ADP-006, DOMAIN.md §10.4 C).

use serde_json::{json, Value};

use super::document;
use super::html;
use super::pdf;
use super::ports::{MeasureSpec, PdfSpec, RenderJob};
use super::quality::{Check, QualityRecord, Trust};
use super::shapes::DIAGRAM_MEDIA_TYPE;
use super::support::{commit_output, read_source, refuse, success, unavailable, Ctx, OutputSpec};
use super::svg::{self, DiagramIr, DiagramLimits};
use crate::artifacts::ArtifactKind;
use crate::runtime::turn_loop::{HostFailure, HostOutcome};

const PDF: &str = "document.export_pdf";
const DIAGRAM: &str = "diagram.render";

/// `document.export_pdf`.
pub async fn export_pdf(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(export_pdf_inner(ctx)
        .await
        .unwrap_or_else(|outcome| outcome))
}

async fn export_pdf_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let config = &ctx.env.config;
    let limits = &config.pdf.document;
    let document_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let body = ctx.arg_str("html").unwrap_or_default().to_string();
    let loaded = read_source(ctx, &document_id, ArtifactKind::Document, None).await?;
    let source = String::from_utf8_lossy(&loaded.bytes).into_owned();
    if body.len() > limits.html_max_bytes {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.document.pdf.size",
            format!(
                "the printable HTML is {} bytes; at most {}",
                body.len(),
                limits.html_max_bytes
            ),
        ));
    }
    html::check_markup(&body).map_err(|refusal| {
        refuse(
            "VALIDATION_SCHEMA",
            refusal.rule,
            format!("the document's HTML: {}", refusal.detail),
        )
    })?;
    let Some(port) = ctx.env.render.clone() else {
        return Err(unavailable(
            "render",
            "no render host is composed on this deployment, so nothing can be printed",
            None,
        ));
    };
    let result = port
        .render(RenderJob {
            html: document::document_shell(&body),
            assets: std::collections::BTreeMap::new(),
            viewport: (816, 1056),
            measure: MeasureSpec::Text,
            pdf: Some(PdfSpec {
                paper_width_in: limits.paper_width_in,
                paper_height_in: limits.paper_height_in,
                margin_in: limits.margin_in,
                prefer_css_page_size: false,
                max_bytes: limits.max_bytes,
            }),
            screenshot: false,
            max_screenshot_bytes: config.render.screenshot_max_bytes,
            deadline_ms: config.render.deadline_ms,
        })
        .await
        .map_err(super::deck_host::render_failure)?;
    let Some(printed) = result.pdf else {
        return Err(refuse(
            "INTERNAL",
            "output.document.pdf",
            "the render produced no PDF",
        ));
    };
    let bytes = pdf::canonicalise(printed);

    let mut record = QualityRecord::new(PDF, Trust::AgentGenerated);
    record.inputs.push(loaded.input());
    record
        .environment
        .insert("browser".to_string(), Value::String(result.browser.clone()));
    let source_words = document::markdown_words(&source);
    let covered = document::coverage(&source_words, &document::words(&result.text));
    record.checks.push(Check::of(
        "output.document.pdf.source_text",
        covered >= limits.min_text_coverage,
        format!(
            "{:.1}% of the source's {} words are on the printed page (at least {:.0}% required)",
            covered * 100.0,
            source_words.values().sum::<usize>(),
            limits.min_text_coverage * 100.0
        ),
    ));
    let mut pages = 0;
    match pdf::inspect(&bytes) {
        Ok(info) => {
            pages = info.pages;
            record.checks.push(Check::passed(
                "output.document.pdf.structure",
                format!(
                    "a PDF {} with a catalog and a page tree, no active content",
                    info.version
                ),
            ));
            record.checks.push(Check::of(
                "output.document.pdf.pages",
                info.pages <= limits.max_pages,
                format!("{} pages (at most {})", info.pages, limits.max_pages),
            ));
        }
        Err(detail) => {
            record
                .checks
                .push(Check::failed("output.document.pdf.structure", detail));
            record.checks.push(Check::failed(
                "output.document.pdf.pages",
                "not checked: the PDF does not parse",
            ));
        }
    }
    record.checks.push(Check::of(
        "output.document.pdf.network",
        result.blocked.is_empty(),
        if result.blocked.is_empty() {
            "the page asked for nothing outside itself".to_string()
        } else {
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
            )
        },
    ));
    if let Some(failure) = record.first_failure() {
        return Err(super::support::refuse_with(
            "VALIDATION_SCHEMA",
            record
                .failures()
                .first()
                .map_or(PDF, |check| check.rule.as_str()),
            format!("the PDF failed its checks, so no version was made: {failure}"),
            Some(json!({ "checks": record.checks.iter().map(Check::to_json).collect::<Vec<_>>() })),
        ));
    }
    let size = bytes.len();
    let committed = commit_output(
        ctx,
        OutputSpec {
            role: format!("document_pdf:{document_id}"),
            kind: ArtifactKind::Other,
            title: format!("{} (PDF)", loaded.artifact.title),
            media_type: "application/pdf".to_string(),
            bytes,
            record,
        },
    )
    .await?;
    let version_id = committed.version.id.to_canonical();
    Ok(success(
        json!({
            "artifact_id": committed.artifact_id.to_canonical(),
            "version_id": version_id,
            "version": committed.version.seq,
            "content_digest": committed.version.content_digest.to_string(),
            "size_bytes": size,
            "pages": pages,
            "replayed": committed.replayed,
            "trust": Trust::AgentGenerated.as_str(),
        }),
        Vec::new(),
        Some(version_id),
    ))
}

/// `diagram.render`.
pub async fn render_diagram(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(render_diagram_inner(ctx)
        .await
        .unwrap_or_else(|outcome| outcome))
}

async fn render_diagram_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let limits = DiagramLimits {
        max_nodes: ctx.env.config.diagram.max_nodes,
        max_edges: ctx.env.config.diagram.max_edges,
        svg_max_bytes: ctx.env.config.diagram.svg_max_bytes,
    };
    let diagram_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let svg_text = ctx.arg_str("svg").unwrap_or_default().to_string();
    let loaded = read_source(
        ctx,
        &diagram_id,
        ArtifactKind::Other,
        Some(DIAGRAM_MEDIA_TYPE),
    )
    .await?;
    let ir = DiagramIr::parse(&loaded.bytes, limits).map_err(|detail| {
        refuse(
            "VALIDATION_SCHEMA",
            "artifact.format",
            format!("the diagram source is not valid: {detail}"),
        )
    })?;
    let mut record = QualityRecord::new(DIAGRAM, Trust::AgentGenerated);
    record.inputs.push(loaded.input());
    record.checks = svg::verify(&svg_text, &ir, limits);
    if let Some(failure) = record.first_failure() {
        return Err(super::support::refuse_with(
            "VALIDATION_SCHEMA",
            record
                .failures()
                .first()
                .map_or(DIAGRAM, |check| check.rule.as_str()),
            format!("the SVG failed its checks, so no version was made: {failure}"),
            Some(json!({ "checks": record.checks.iter().map(Check::to_json).collect::<Vec<_>>() })),
        ));
    }
    let committed = commit_output(
        ctx,
        OutputSpec {
            role: format!("diagram_svg:{diagram_id}"),
            kind: ArtifactKind::Image,
            title: format!("{} (SVG)", ir.title),
            media_type: "image/svg+xml".to_string(),
            bytes: svg_text.into_bytes(),
            record,
        },
    )
    .await?;
    let version_id = committed.version.id.to_canonical();
    Ok(success(
        json!({
            "artifact_id": committed.artifact_id.to_canonical(),
            "version_id": version_id,
            "version": committed.version.seq,
            "content_digest": committed.version.content_digest.to_string(),
            "replayed": committed.replayed,
            "trust": Trust::AgentGenerated.as_str(),
        }),
        Vec::new(),
        Some(version_id),
    ))
}
