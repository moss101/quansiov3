//! The format check of what the output tools make (ADP-006, DOMAIN.md §10.4 A).
//!
//! Every version the store adds records a `format_check` (`artifacts::formats::check`). For the
//! media types the output tools write, the check is made here, so an upload or a command that
//! stores such bytes gets the same verdict an output tool would have refused on, and a reader of
//! the artifact can tell a sound one from a malformed one without trusting who stored it.

use super::config::OutputsConfig;
use super::deck::{DeckPages, PAGES_MEDIA_TYPE, PDF_MEDIA_TYPE, PPTX_MEDIA_TYPE};
use super::ooxml::{self, PackageLimits};
use super::pdf;
use super::svg::{self, DiagramIr, DiagramLimits};
use crate::artifacts::ArtifactKind;

/// The diagram IR's media type.
pub const DIAGRAM_MEDIA_TYPE: &str = "application/vnd.quansio.diagram+json";

/// The verdict for a media type this module owns: `Some(Ok)` holds, `Some(Err((rule, detail)))`
/// does not, `None` when the media type is not one of its own.
#[must_use]
pub fn check(
    kind: ArtifactKind,
    media_type: &str,
    payload: &[u8],
) -> Option<Result<(), (&'static str, String)>> {
    let config = shipped();
    let format = |detail: String| Err((super::super::formats::RULE_FORMAT, detail));
    let media = |detail: String| Err((super::super::formats::RULE_MEDIA, detail));
    let other_only = |outcome: Result<(), (&'static str, String)>| {
        if kind == ArtifactKind::Other {
            outcome
        } else {
            media(format!(
                "{media_type} is the media type of an `other` artifact, not of {}",
                kind.as_str()
            ))
        }
    };
    match media_type {
        PAGES_MEDIA_TYPE => Some(other_only(
            DeckPages::parse(payload).map(|_| ()).or_else(format),
        )),
        PDF_MEDIA_TYPE if kind == ArtifactKind::Other => {
            Some(pdf::inspect(payload).map(|_| ()).or_else(format))
        }
        PPTX_MEDIA_TYPE => Some(other_only(
            ooxml::inspect_package(
                payload,
                PackageLimits {
                    max_parts: config.deck.pptx.max_parts,
                    max_part_bytes: config.deck.pptx.max_part_bytes,
                    max_bytes: config.deck.pptx.max_bytes,
                },
            )
            .map(|_| ())
            .or_else(format),
        )),
        DIAGRAM_MEDIA_TYPE => Some(other_only(
            DiagramIr::parse(
                payload,
                DiagramLimits {
                    max_nodes: config.diagram.max_nodes,
                    max_edges: config.diagram.max_edges,
                    svg_max_bytes: config.diagram.svg_max_bytes,
                },
            )
            .map(|_| ())
            .or_else(format),
        )),
        "image/svg+xml" if kind == ArtifactKind::Image => {
            let verdict = svg::check_safe(payload, config.diagram.svg_max_bytes);
            Some(if verdict.ok {
                Ok(())
            } else {
                format(verdict.detail)
            })
        }
        _ => None,
    }
}

fn shipped() -> &'static OutputsConfig {
    static SHIPPED: std::sync::OnceLock<OutputsConfig> = std::sync::OnceLock::new();
    SHIPPED.get_or_init(OutputsConfig::shipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_this_module_owns_are_checked_and_others_are_left_alone() {
        assert!(check(ArtifactKind::Other, "text/plain", b"x").is_none());
        assert!(check(ArtifactKind::Document, "text/markdown", b"# t").is_none());
        assert!(check(ArtifactKind::Image, "image/png", b"12345678").is_none());
        // An image that is not an SVG is not this module's; an `application/pdf` of another kind is not either.
        assert!(check(ArtifactKind::Document, PDF_MEDIA_TYPE, b"x").is_none());
    }

    #[test]
    fn malformed_bytes_of_an_owned_media_type_fail_with_the_format_rule() {
        let verdict = check(ArtifactKind::Other, PDF_MEDIA_TYPE, b"not a pdf").expect("owned");
        assert_eq!(verdict.expect_err("fails").0, "artifact.format");
        let verdict = check(ArtifactKind::Other, PAGES_MEDIA_TYPE, b"{}").expect("owned");
        assert!(verdict.is_err());
        let verdict = check(ArtifactKind::Other, PPTX_MEDIA_TYPE, b"PK nope").expect("owned");
        assert!(verdict.is_err());
        let verdict = check(ArtifactKind::Image, "image/svg+xml", b"<svg/>").expect("owned");
        assert!(verdict.is_err(), "no viewBox and no namespace");
        let verdict = check(ArtifactKind::Document, PAGES_MEDIA_TYPE, b"{}").expect("owned");
        assert_eq!(verdict.expect_err("kind").0, "artifact.media_type");
    }

    #[test]
    fn a_sound_diagram_source_and_svg_pass() {
        let ir = br#"{"schema_version":"v1","kind":"flow","title":"t","nodes":[{"id":"a","label":"A"}],"edges":[]}"#;
        assert!(check(ArtifactKind::Other, DIAGRAM_MEDIA_TYPE, ir)
            .expect("owned")
            .is_ok());
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><rect width=\"5\" height=\"5\"/></svg>";
        assert!(check(ArtifactKind::Image, "image/svg+xml", svg)
            .expect("owned")
            .is_ok());
    }
}
