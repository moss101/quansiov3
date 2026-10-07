//! Artifacts only the output tools may write (ADP-006, DOMAIN.md §10.4 A).
//!
//! A deck's pages, a rendered video, a transcript, a diagram's SVG and a preview's suggestions are
//! results the authority produced and checked. `artifact.create` and `artifact.update` therefore
//! refuse their roles and media types, so a model cannot make an artifact that looks like the
//! authority's own output. (An upload or a command can still store such bytes, but a version made
//! that way has no quality record, and every reader of these artifacts requires one.)

/// Role prefixes reserved for the output tools. A role is `<prefix>` or `<prefix>:<source>`.
pub const ROLE_PREFIXES: &[&str] = &[
    "deck_pages",
    "deck_pdf",
    "deck_pptx",
    "diagram_svg",
    "document_pdf",
    "transcript",
    "video_render",
    "video_contact_sheet",
    "preview_suggestions",
];

/// Media types only an output tool may store through the artifact tools.
pub const MEDIA_TYPES: &[&str] = &[
    "application/vnd.quansio.deck-pages+json",
    "application/vnd.quansio.transcript+json",
];

/// The media type of a video timeline: created through `artifact.create`, edited only by `video.edit`.
pub const TIMELINE_MEDIA_TYPE: &str = "application/vnd.quansio.video-timeline+json";

/// Whether a role is reserved.
#[must_use]
pub fn reserved_role(role: &str) -> bool {
    ROLE_PREFIXES.iter().any(|prefix| {
        role == *prefix
            || role
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with(':'))
    })
}

/// Why `artifact.create` must refuse this role and media type, if it must.
#[must_use]
pub fn refuses_create(role: &str, media_type: &str) -> Option<String> {
    if reserved_role(role) {
        return Some(format!(
            "the role {role:?} belongs to the output tools; they make that artifact, `artifact.create` cannot"
        ));
    }
    if MEDIA_TYPES.contains(&media_type) {
        return Some(format!(
            "{media_type} is written only by an output tool; `artifact.create` cannot make one"
        ));
    }
    None
}

/// Why `artifact.update` must refuse this artifact or media type, if it must.
#[must_use]
pub fn refuses_update(
    role: Option<&str>,
    current_media_type: &str,
    new_media_type: &str,
) -> Option<String> {
    if let Some(role) = role.filter(|role| reserved_role(role)) {
        return Some(format!(
            "the artifact's role {role:?} belongs to the output tools; `artifact.update` cannot add a version"
        ));
    }
    if MEDIA_TYPES.contains(&current_media_type) || MEDIA_TYPES.contains(&new_media_type) {
        return Some(format!(
            "{current_media_type} is written only by an output tool; `artifact.update` cannot change it"
        ));
    }
    if current_media_type == TIMELINE_MEDIA_TYPE || new_media_type == TIMELINE_MEDIA_TYPE {
        return Some(
            "a video timeline changes only through `video.edit`, which checks the revision and applies a batch atomically"
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_role_is_the_prefix_or_the_prefix_with_a_source() {
        assert!(reserved_role("deck_pages"));
        assert!(reserved_role("deck_pages:art_X"));
        assert!(reserved_role("transcript:req_1"));
        assert!(!reserved_role("deck_pages2"));
        assert!(!reserved_role("deck"));
        assert!(!reserved_role("report"));
        assert!(!reserved_role("my_transcript"));
    }

    #[test]
    fn create_and_update_refuse_the_tools_artifacts() {
        assert!(refuses_create("deck_pdf:art_X", "application/pdf").is_some());
        assert!(refuses_create("report", "application/vnd.quansio.deck-pages+json").is_some());
        assert!(refuses_create("report", "application/vnd.quansio.transcript+json").is_some());
        assert!(refuses_create("report", "application/vnd.quansio.video-timeline+json").is_none());
        assert!(refuses_create("report", "text/markdown").is_none());

        assert!(refuses_update(
            Some("deck_pptx:art_X"),
            "application/pdf",
            "application/pdf"
        )
        .is_some());
        assert!(refuses_update(
            Some("report"),
            "application/vnd.quansio.transcript+json",
            "text/plain"
        )
        .is_some());
        assert!(refuses_update(
            Some("report"),
            "text/plain",
            "application/vnd.quansio.deck-pages+json"
        )
        .is_some());
        assert!(refuses_update(
            Some("video_timeline"),
            TIMELINE_MEDIA_TYPE,
            TIMELINE_MEDIA_TYPE
        )
        .is_some());
        assert!(refuses_update(None, "text/markdown", "text/markdown").is_none());
        assert!(refuses_update(Some("report"), "text/markdown", "text/plain").is_none());
    }
}
