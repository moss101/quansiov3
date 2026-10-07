//! Artifact source formats and quality checks (CAP-003, DOMAIN.md §10.1).
//!
//! An editable artifact is a versioned source file with a closed shape, not a chat blob:
//!
//! | kind | media types | shape |
//! |---|---|---|
//! | `document` | `text/markdown`, `text/plain` | UTF-8, first non-blank line `# Title`, no active content |
//! | `spreadsheet` | `text/csv`, `text/tab-separated-values` | CSV, unique non-empty headers, rectangular, ≥ 1 row |
//! | `presentation` | `application/vnd.quansio.slides+json` | `{schema_version: "v1", slides: [{title, body?, notes?}]}` |
//! | `code` | `application/vnd.quansio.code+json` | `{schema_version: "v1", language, files: [{path, content}]}` |
//! | `data` | `application/json` | `{schema_version: "v1", columns: [..], rows: [{column: value}]}` |
//!
//! Media kinds (`image`, `audio`, `video`) need a matching media type and real bytes; other
//! kinds only refuse an empty payload or a chat transcript. The quality checks then ask for
//! substance (a document body, a body on every slide, content in every file).
//!
//! The intelligence plane (`python/intelligence/artifacts/formats.py`) applies the same rules
//! before it proposes a source; these are the authority's, applied to every version the store
//! records (`artifact_versions.format_check`) and by the `artifact.create`/`artifact.update`
//! tool host. `packs/skills/artifacts/fixtures/` is the corpus both sides must agree on.

use serde::Serialize;
use serde_json::{Map, Value};

use super::ArtifactKind;

/// The rule a failed check names.
pub const RULE_KIND: &str = "artifact.kind";
/// The source is not the kind's shape.
pub const RULE_FORMAT: &str = "artifact.format";
/// The source has the shape but not the substance.
pub const RULE_QUALITY: &str = "artifact.quality";
/// The source is a chat transcript posing as an artifact.
pub const RULE_OPAQUE: &str = "artifact.opaque_blob";
/// The media type does not fit the kind.
pub const RULE_MEDIA: &str = "artifact.media_type";

/// The only source schema version.
pub const SCHEMA_VERSION: &str = "v1";

const DOCUMENT_SCRIPT_MARKERS: [&str; 5] =
    ["<script", "</script", "javascript:", "onerror=", "onload="];

/// The outcome of validating one version (`artifact_versions.format_check`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatCheck {
    /// Whether format and quality both hold.
    pub ok: bool,
    /// The rule that decided (`artifact.format` … or `artifact.quality` when it holds).
    pub rule: String,
    /// What was found.
    pub detail: String,
}

impl FormatCheck {
    fn failed(rule: &str, detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            rule: rule.to_string(),
            detail: detail.into(),
        }
    }

    /// The JSON the version row stores.
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

type Refusal = (&'static str, String);

fn format(detail: impl Into<String>) -> Refusal {
    (RULE_FORMAT, detail.into())
}

/// Whether a kind is an editable source kind.
#[must_use]
pub const fn is_editable(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::Document
            | ArtifactKind::Spreadsheet
            | ArtifactKind::Presentation
            | ArtifactKind::Code
            | ArtifactKind::Data
    )
}

/// The media type a kind's source has when the caller does not name one.
#[must_use]
pub const fn default_media_type(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Document => "text/markdown",
        ArtifactKind::Spreadsheet => "text/csv",
        ArtifactKind::Presentation => "application/vnd.quansio.slides+json",
        ArtifactKind::Code => "application/vnd.quansio.code+json",
        ArtifactKind::Data => "application/json",
        ArtifactKind::Image => "image/png",
        ArtifactKind::Audio => "audio/mpeg",
        ArtifactKind::Video => "video/mp4",
        ArtifactKind::Archive => "application/zip",
        ArtifactKind::Other => "application/octet-stream",
    }
}

fn check_media_type(kind: ArtifactKind, media_type: &str) -> Result<(), Refusal> {
    let media_type = media_type.trim();
    if media_type.is_empty() {
        return Err((RULE_MEDIA, "media type is required".to_string()));
    }
    let fits = match kind {
        ArtifactKind::Document => matches!(media_type, "text/markdown" | "text/plain"),
        ArtifactKind::Spreadsheet => {
            matches!(media_type, "text/csv" | "text/tab-separated-values")
        }
        ArtifactKind::Image => media_type.starts_with("image/"),
        ArtifactKind::Audio => media_type.starts_with("audio/"),
        ArtifactKind::Video => media_type.starts_with("video/"),
        _ => true,
    };
    if fits {
        Ok(())
    } else {
        Err((
            RULE_MEDIA,
            format!(
                "{media_type:?} is not a media type a {} source has",
                kind.as_str()
            ),
        ))
    }
}

fn looks_like_message(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        !map.contains_key("schema_version")
            && map.contains_key("role")
            && map.contains_key("content")
    })
}

fn refuse_chat_blob(payload: &[u8]) -> Result<(), Refusal> {
    let Ok(decoded) = serde_json::from_slice::<Value>(payload) else {
        return Ok(());
    };
    let transcript = looks_like_message(&decoded)
        || decoded
            .as_array()
            .is_some_and(|items| !items.is_empty() && items.iter().all(looks_like_message));
    if transcript {
        return Err((
            RULE_OPAQUE,
            "the source is a chat transcript; an artifact is a versioned source file".to_string(),
        ));
    }
    Ok(())
}

fn utf8<'a>(payload: &'a [u8], label: &str) -> Result<&'a str, Refusal> {
    std::str::from_utf8(payload).map_err(|_| format(format!("{label} is not UTF-8")))
}

fn json_object(payload: &[u8], label: &str) -> Result<Map<String, Value>, Refusal> {
    match serde_json::from_slice::<Value>(payload) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(format(format!("{label} must be a JSON object"))),
        Err(_) => Err(format(format!("{label} is not JSON"))),
    }
}

fn closed_keys(map: &Map<String, Value>, allowed: &[&str], label: &str) -> Result<(), Refusal> {
    let mut unknown: Vec<&str> = map
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    unknown.sort_unstable();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format(format!(
            "{label} has unknown keys: {}",
            unknown.join(", ")
        )))
    }
}

fn schema_version(map: &Map<String, Value>, label: &str) -> Result<(), Refusal> {
    if map.get("schema_version").and_then(Value::as_str) == Some(SCHEMA_VERSION) {
        Ok(())
    } else {
        Err(format(format!("{label} schema_version must be v1")))
    }
}

/// Parse CSV (RFC 4180: quoted fields, doubled quotes, separators and newlines inside
/// quotes). A blank final line is not a row.
fn csv_rows(text: &str) -> Result<Vec<Vec<String>>, Refusal> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut row_started = false;
    while let Some(ch) = chars.next() {
        if quoted {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                other => field.push(other),
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                row_started = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                row_started = true;
            }
            '\r' => {}
            '\n' => {
                if row_started || !field.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                } else {
                    rows.push(Vec::new());
                }
                row_started = false;
            }
            other => {
                field.push(other);
                row_started = true;
            }
        }
    }
    if quoted {
        return Err(format("spreadsheet has an unterminated quoted field"));
    }
    if row_started || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn document(payload: &[u8]) -> Result<Option<Refusal>, Refusal> {
    let text = utf8(payload, "document")?;
    let lowered = text.to_lowercase();
    if let Some(marker) = DOCUMENT_SCRIPT_MARKERS
        .iter()
        .find(|m| lowered.contains(**m))
    {
        return Err(format(format!(
            "document contains active content marker {marker:?}"
        )));
    }
    let lines: Vec<&str> = text.lines().collect();
    let Some(heading_index) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return Err(format(
            "document must start with a markdown '# Title' heading",
        ));
    };
    let Some(title) = lines[heading_index].strip_prefix("# ") else {
        return Err(format(
            "document must start with a markdown '# Title' heading",
        ));
    };
    if title.trim().is_empty() {
        return Err(format("document title is empty"));
    }
    let body = lines[heading_index + 1..].join("\n");
    Ok((body.trim().is_empty()).then(|| (RULE_QUALITY, "document body is empty".to_string())))
}

fn spreadsheet(payload: &[u8]) -> Result<Option<Refusal>, Refusal> {
    let rows = csv_rows(utf8(payload, "spreadsheet")?)?;
    let Some(header) = rows.first() else {
        return Err(format("spreadsheet has no header row"));
    };
    let headers: Vec<&str> = header.iter().map(|cell| cell.trim()).collect();
    if headers.is_empty() || headers.iter().any(|header| header.is_empty()) {
        return Err(format("spreadsheet headers must be non-empty"));
    }
    let mut unique = headers.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != headers.len() {
        return Err(format("spreadsheet headers must be unique"));
    }
    for (index, row) in rows.iter().enumerate().skip(1) {
        if row.len() != headers.len() {
            return Err(format(format!(
                "spreadsheet row {} has {} columns, expected {}",
                index + 1,
                row.len(),
                headers.len()
            )));
        }
    }
    if rows.len() < 2 {
        return Err(format("spreadsheet needs at least one data row"));
    }
    Ok(None)
}

fn presentation(payload: &[u8]) -> Result<Option<Refusal>, Refusal> {
    // The slide shape, with the optional layout, alt, image and chart keys (DOMAIN.md §10.4 B), is one
    // parser shared with the deck tools, so a deck the store accepts is a deck the deck tools can read.
    let image_max = super::outputs::config::OutputsConfig::shipped()
        .deck
        .image_max_bytes;
    let source = super::outputs::deck::parse_presentation(payload, image_max).map_err(format)?;
    if source
        .slides
        .iter()
        .any(|slide| slide.body.trim().is_empty())
    {
        return Ok(Some((RULE_QUALITY, "every slide needs a body".to_string())));
    }
    Ok(None)
}

fn safe_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\0')
        && !path.split(['/', '\\']).any(|segment| segment == "..")
}

fn code(payload: &[u8]) -> Result<Option<Refusal>, Refusal> {
    let map = json_object(payload, "code")?;
    closed_keys(
        &map,
        &["schema_version", "language", "files"],
        "code artifact",
    )?;
    schema_version(&map, "code")?;
    if map
        .get("language")
        .and_then(Value::as_str)
        .is_none_or(|language| language.trim().is_empty())
    {
        return Err(format("code artifact needs a language"));
    }
    let files = map
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| format("code files must be a list"))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut quality = None;
    for (index, file) in files.iter().enumerate() {
        let file = file
            .as_object()
            .ok_or_else(|| format(format!("code file {index} is not an object")))?;
        closed_keys(file, &["path", "content"], &format!("code file {index}"))?;
        let path = file.get("path").and_then(Value::as_str).unwrap_or("");
        if path.trim().is_empty() {
            return Err(format(format!("code file {index} needs a path")));
        }
        if !safe_path(path) {
            return Err(format(format!(
                "code file {index} path {path:?} must be relative and stay inside the artifact"
            )));
        }
        let Some(content) = file.get("content").and_then(Value::as_str) else {
            return Err(format(format!("code file {index} content must be text")));
        };
        if !seen.insert(path.to_string()) {
            return Err(format(format!("duplicate code path {path:?}")));
        }
        if content.trim().is_empty() && quality.is_none() {
            quality = Some((RULE_QUALITY, "every code file needs content".to_string()));
        }
    }
    if files.is_empty() {
        return Err(format("code artifact needs at least one file"));
    }
    Ok(quality)
}

fn data(payload: &[u8]) -> Result<Option<Refusal>, Refusal> {
    let map = json_object(payload, "data")?;
    closed_keys(
        &map,
        &["schema_version", "rows", "columns"],
        "data artifact",
    )?;
    schema_version(&map, "data")?;
    let columns: Vec<String> = map
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| format("data columns must be a list"))?
        .iter()
        .map(|column| match column {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect();
    if columns.is_empty() || columns.iter().any(|column| column.trim().is_empty()) {
        return Err(format("data columns must be non-empty names"));
    }
    let rows = map
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| format("data rows must be a list"))?;
    for (index, row) in rows.iter().enumerate() {
        let row = row
            .as_object()
            .ok_or_else(|| format(format!("data row {index} is not an object")))?;
        let mut extra: Vec<&str> = row
            .keys()
            .map(String::as_str)
            .filter(|key| !columns.iter().any(|column| column == key))
            .collect();
        extra.sort_unstable();
        if !extra.is_empty() {
            return Err(format(format!(
                "data row {index} has unknown columns: {}",
                extra.join(", ")
            )));
        }
    }
    if rows.is_empty() {
        return Err(format("data artifact needs at least one row"));
    }
    Ok(None)
}

/// Validate one version's bytes: media type, shape, then quality.
#[must_use]
pub fn check(kind: ArtifactKind, media_type: &str, payload: &[u8]) -> FormatCheck {
    let outcome = check_media_type(kind, media_type).and_then(|()| {
        refuse_chat_blob(payload)?;
        if let Some(verdict) = super::outputs::shapes::check(kind, media_type, payload) {
            return verdict.map(|()| None);
        }
        match kind {
            ArtifactKind::Document => document(payload),
            ArtifactKind::Spreadsheet => spreadsheet(payload),
            ArtifactKind::Presentation => presentation(payload),
            ArtifactKind::Code => code(payload),
            ArtifactKind::Data => data(payload),
            ArtifactKind::Image | ArtifactKind::Audio | ArtifactKind::Video => {
                if payload.len() < 8 {
                    Ok(Some((
                        RULE_QUALITY,
                        "media payload is too small to be real".to_string(),
                    )))
                } else {
                    Ok(None)
                }
            }
            ArtifactKind::Archive | ArtifactKind::Other => {
                if payload.is_empty() {
                    Err(format(format!("{} payload is empty", kind.as_str())))
                } else {
                    Ok(None)
                }
            }
        }
    });
    match outcome {
        Ok(None) => FormatCheck {
            ok: true,
            rule: RULE_QUALITY.to_string(),
            detail: format!("{} format and quality hold", kind.as_str()),
        },
        Ok(Some((rule, detail))) | Err((rule, detail)) => FormatCheck::failed(rule, detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(kind: ArtifactKind, body: &str) -> bool {
        check(kind, default_media_type(kind), body.as_bytes()).ok
    }

    fn rule(kind: ArtifactKind, body: &str) -> String {
        check(kind, default_media_type(kind), body.as_bytes()).rule
    }

    #[test]
    fn documents_need_a_title_a_body_and_no_active_content() {
        assert!(ok(ArtifactKind::Document, "# Plan\n\nShip it.\n"));
        assert_eq!(
            rule(ArtifactKind::Document, "Plan\n\nShip it."),
            RULE_FORMAT
        );
        assert_eq!(rule(ArtifactKind::Document, "# Plan\n"), RULE_QUALITY);
        assert_eq!(
            rule(ArtifactKind::Document, "# Plan\n<script>x</script>"),
            RULE_FORMAT
        );
        assert_eq!(
            rule(
                ArtifactKind::Document,
                r#"{"role":"assistant","content":"hi"}"#
            ),
            RULE_OPAQUE
        );
        assert_eq!(
            check(ArtifactKind::Document, "text/html", b"# Plan\n\nx").rule,
            RULE_MEDIA
        );
    }

    #[test]
    fn spreadsheets_are_rectangular_csv_with_unique_headers() {
        assert!(ok(
            ArtifactKind::Spreadsheet,
            "name,qty\n\"a, b\",1\n\"say \"\"hi\"\"\",2\n"
        ));
        assert!(!ok(ArtifactKind::Spreadsheet, "name,qty\na\n"));
        assert!(!ok(ArtifactKind::Spreadsheet, "name,name\na,b\n"));
        assert!(!ok(ArtifactKind::Spreadsheet, "name,qty\n"));
        assert!(!ok(ArtifactKind::Spreadsheet, "name,qty\n\"open,1\n"));
    }

    #[test]
    fn slides_code_and_data_are_closed_json_shapes() {
        assert!(ok(
            ArtifactKind::Presentation,
            r#"{"schema_version":"v1","slides":[{"title":"A","body":"b"}]}"#
        ));
        assert_eq!(
            rule(
                ArtifactKind::Presentation,
                r#"{"schema_version":"v1","slides":[{"title":"A"}]}"#
            ),
            RULE_QUALITY
        );
        assert!(!ok(
            ArtifactKind::Presentation,
            r#"{"schema_version":"v1","slides":[{"title":"A","body":"b","x":1}]}"#
        ));
        assert!(ok(
            ArtifactKind::Code,
            r#"{"schema_version":"v1","language":"rust","files":[{"path":"src/lib.rs","content":"fn a() {}"}]}"#
        ));
        assert!(!ok(
            ArtifactKind::Code,
            r#"{"schema_version":"v1","language":"rust","files":[{"path":"../etc/passwd","content":"x"}]}"#
        ));
        assert!(ok(
            ArtifactKind::Data,
            r#"{"schema_version":"v1","columns":["a"],"rows":[{"a":1}]}"#
        ));
        assert!(!ok(
            ArtifactKind::Data,
            r#"{"schema_version":"v1","columns":["a"],"rows":[{"b":1}]}"#
        ));
    }

    #[test]
    fn media_needs_its_media_type_and_real_bytes() {
        assert!(check(ArtifactKind::Image, "image/png", &[0x89; 64]).ok);
        assert_eq!(
            check(ArtifactKind::Image, "text/plain", &[0x89; 64]).rule,
            RULE_MEDIA
        );
        assert_eq!(
            check(ArtifactKind::Image, "image/png", &[1, 2]).rule,
            RULE_QUALITY
        );
    }
}
