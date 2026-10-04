//! The recording archive (DOMAIN.md §11.7 C): validation on the way in, and the text-only projection that
//! goes out to the plane.
//!
//! An archive is what the person's recorder built *after* redaction. The server does not take that on
//! trust: before an archive is stored it must have the closed shape below, stay inside the configured
//! limits, belong to the recording and owner that sent it, and hold nothing that the redaction rule set
//! would still remove. A violation stores nothing and names the categories and JSON pointers — never the
//! matched text — so the refusal itself cannot leak what it refused.
//!
//! The projection is what a compiler may read: actions, narration, recognised screen text and window
//! titles. It carries no pixel, no screen point and no hash of either.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::config::RecordingConfig;
use super::redact::{field_rule, scan_text, Category, Counts, FieldAttrs, FieldRule, RULE_SET};
use super::RecordingError;

/// The media type of the stored archive.
pub const ARCHIVE_MEDIA_TYPE: &str = "application/vnd.quansio.recording+json";
/// The only archive schema version.
pub const ARCHIVE_SCHEMA_VERSION: &str = "v1";
/// The platforms a recorder reports.
pub const PLATFORMS: [&str; 1] = ["macos"];
/// The action kinds a recorder reports.
pub const ACTION_KINDS: [&str; 10] = [
    "click",
    "double_click",
    "right_click",
    "type",
    "key",
    "scroll",
    "focus",
    "app_switch",
    "menu_select",
    "set_value",
];
/// Why capture paused.
pub const PAUSE_REASONS: [&str; 3] = ["secure_input", "secure_field", "user_pause"];
/// Pointers a refusal names at most.
const MAX_POINTERS: usize = 20;

static BUNDLE_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)+$",
    )
    .expect("bundle id pattern")
});

/// Who an archive must belong to.
#[derive(Debug, Clone)]
pub struct Expected<'a> {
    /// The recording the archive finishes.
    pub recording_id: &'a str,
    /// Its tenant.
    pub tenant_id: &'a str,
    /// Its owner.
    pub user_id: &'a str,
    /// Its epoch.
    pub epoch: i64,
}

/// What a valid archive yields.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchiveReport {
    /// `{rule_set, counts, screenshots_withheld}`.
    pub redaction: Value,
    /// `{actions, screenshots, transcript_segments, paused_ms}`.
    pub stats: Value,
    /// What the compiler may read.
    pub projection: Value,
}

fn invalid(detail: impl Into<String>) -> RecordingError {
    RecordingError::InvalidArchive(detail.into())
}

fn bounds(detail: impl Into<String>) -> RecordingError {
    RecordingError::ArchiveBounds(detail.into())
}

fn object<'a>(
    value: &'a Value,
    what: &str,
    keys: &[&str],
) -> Result<&'a Map<String, Value>, RecordingError> {
    let map = value
        .as_object()
        .ok_or_else(|| invalid(format!("{what} must be an object")))?;
    if let Some(unknown) = map.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(invalid(format!("{what} has an unknown field `{unknown}`")));
    }
    Ok(map)
}

fn string<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<&'a str, RecordingError> {
    map.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{what}.{key} must be text")))
}

fn optional_string<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<Option<&'a str>, RecordingError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid(format!("{what}.{key} must be text"))),
    }
}

fn timestamp(
    map: &Map<String, Value>,
    key: &str,
    what: &str,
) -> Result<DateTime<Utc>, RecordingError> {
    let text = string(map, key, what)?;
    DateTime::parse_from_rfc3339(text)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| invalid(format!("{what}.{key} must be an RFC 3339 time")))
}

fn within(text: &str, max: usize, what: &str) -> Result<(), RecordingError> {
    if text.chars().count() > max {
        Err(bounds(format!("{what} is longer than {max} characters")))
    } else {
        Ok(())
    }
}

/// Where redaction would still find something, by JSON pointer.
#[derive(Debug, Default)]
struct Findings {
    categories: BTreeSet<Category>,
    pointers: Vec<String>,
}

impl Findings {
    fn text(&mut self, pointer: &str, text: &str) {
        let counts = scan_text(text);
        self.note(pointer, &counts);
    }

    fn note(&mut self, pointer: &str, counts: &Counts) {
        if counts.is_empty() {
            return;
        }
        self.categories.extend(counts.categories());
        if self.pointers.len() < MAX_POINTERS {
            self.pointers.push(pointer.to_string());
        }
    }

    fn category(&mut self, pointer: &str, category: Category) {
        self.categories.insert(category);
        if self.pointers.len() < MAX_POINTERS {
            self.pointers.push(pointer.to_string());
        }
    }
}

fn app(
    value: &Value,
    what: &str,
    config: &RecordingConfig,
) -> Result<(String, String), RecordingError> {
    let map = object(value, what, &["bundle_id", "name"])?;
    let bundle_id = string(map, "bundle_id", what)?;
    if bundle_id.len() > 255 || !BUNDLE_ID.is_match(bundle_id) {
        return Err(invalid(format!(
            "{what}.bundle_id is not a bundle identifier"
        )));
    }
    let name = string(map, "name", what)?;
    within(name, config.attribute_max_chars, &format!("{what}.name"))?;
    Ok((bundle_id.to_string(), name.to_string()))
}

const TARGET_KEYS: [&str; 5] = [
    "ax_role",
    "ax_subrole",
    "ax_title",
    "ax_identifier",
    "ax_description",
];

/// Validate an archive and build what the server keeps and what the compiler reads.
///
/// # Errors
/// [`RecordingError::ArchiveBounds`] over a limit, [`RecordingError::InvalidArchive`] for a shape or
/// ownership fault, [`RecordingError::Unredacted`] when redaction would still remove something.
#[allow(clippy::too_many_lines)]
pub fn validate(
    bytes: &[u8],
    expected: &Expected<'_>,
    config: &RecordingConfig,
) -> Result<ArchiveReport, RecordingError> {
    if bytes.len() > config.archive_max_bytes {
        return Err(bounds(format!(
            "the archive is {} bytes; the limit is {}",
            bytes.len(),
            config.archive_max_bytes
        )));
    }
    let document: Value =
        serde_json::from_slice(bytes).map_err(|error| invalid(format!("not JSON: {error}")))?;
    let root = object(
        &document,
        "archive",
        &[
            "schema_version",
            "recording_id",
            "owner",
            "platform",
            "started_at",
            "finished_at",
            "finish",
            "apps",
            "actions",
            "screenshots",
            "transcript",
            "paused",
            "redaction",
            "limits",
        ],
    )?;
    if string(root, "schema_version", "archive")? != ARCHIVE_SCHEMA_VERSION {
        return Err(invalid("archive.schema_version must be v1"));
    }
    if string(root, "recording_id", "archive")? != expected.recording_id {
        return Err(invalid("the archive is for another recording"));
    }
    let owner = object(
        root.get("owner").unwrap_or(&Value::Null),
        "archive.owner",
        &["tenant_id", "user_id", "epoch"],
    )?;
    if string(owner, "tenant_id", "owner")? != expected.tenant_id
        || string(owner, "user_id", "owner")? != expected.user_id
        || owner.get("epoch").and_then(Value::as_i64) != Some(expected.epoch)
    {
        return Err(RecordingError::OwnerChanged {
            reason: "the archive was built for another owner or epoch",
        });
    }
    if !PLATFORMS.contains(&string(root, "platform", "archive")?) {
        return Err(invalid(
            "archive.platform is not a platform a recorder reports",
        ));
    }
    let started = timestamp(root, "started_at", "archive")?;
    let finished = timestamp(root, "finished_at", "archive")?;
    if finished < started {
        return Err(invalid("the archive finishes before it starts"));
    }
    let finish = object(
        root.get("finish").unwrap_or(&Value::Null),
        "archive.finish",
        &["by", "request_id"],
    )?;
    if string(finish, "by", "finish")? != "user" {
        return Err(invalid(
            "a recording is finished by the person: finish.by must be `user`",
        ));
    }
    string(finish, "request_id", "finish")?;

    let mut findings = Findings::default();

    // Applications.
    let mut apps = Vec::new();
    for (index, entry) in root
        .get("apps")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("archive.apps must be a list"))?
        .iter()
        .enumerate()
    {
        let (bundle_id, name) = app(entry, "archive.apps[]", config)?;
        findings.text(&format!("/apps/{index}/name"), &name);
        apps.push(json!({ "bundle_id": bundle_id, "name": name }));
    }

    // Actions.
    let actions = root
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("archive.actions must be a list"))?;
    if actions.len() > config.max_actions {
        return Err(bounds(format!(
            "{} actions; the limit is {}",
            actions.len(),
            config.max_actions
        )));
    }
    let mut projected_actions = Vec::with_capacity(actions.len());
    let mut last_seq = 0u64;
    for (index, entry) in actions.iter().enumerate() {
        let what = "archive.actions[]";
        let map = object(
            entry,
            what,
            &[
                "seq",
                "at",
                "kind",
                "app",
                "target",
                "window_title",
                "target_unresolved",
                "point",
                "text",
                "key",
                "secure",
            ],
        )?;
        let seq = map
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("an action needs a numeric seq"))?;
        if seq <= last_seq {
            return Err(invalid("action seq must increase"));
        }
        last_seq = seq;
        let at = timestamp(map, "at", what)?;
        if at < started || at > finished {
            return Err(invalid("an action is outside the recording's interval"));
        }
        let kind = string(map, "kind", what)?;
        if !ACTION_KINDS.contains(&kind) {
            return Err(invalid(format!("`{kind}` is not an action kind")));
        }
        let (bundle_id, name) = app(map.get("app").unwrap_or(&Value::Null), "action.app", config)?;
        findings.text(&format!("/actions/{index}/app/name"), &name);

        let mut target_projection = Value::Null;
        let mut attrs: [Option<&str>; 5] = [None; 5];
        if let Some(target) = map.get("target").filter(|value| !value.is_null()) {
            let target_map = object(target, "action.target", &TARGET_KEYS)?;
            let mut projected = Map::new();
            for (slot, key) in TARGET_KEYS.iter().enumerate() {
                if let Some(text) = optional_string(target_map, key, "action.target")? {
                    within(
                        text,
                        config.attribute_max_chars,
                        &format!("action.target.{key}"),
                    )?;
                    findings.text(&format!("/actions/{index}/target/{key}"), text);
                    attrs[slot] = Some(text);
                    projected.insert((*key).to_string(), json!(text));
                }
            }
            target_projection = Value::Object(projected);
        }
        let window_title = optional_string(map, "window_title", what)?;
        if let Some(title) = window_title {
            within(title, config.attribute_max_chars, "action.window_title")?;
            findings.text(&format!("/actions/{index}/window_title"), title);
        }
        let unresolved = map.get("target_unresolved").map_or(Ok(false), |value| {
            value
                .as_bool()
                .ok_or_else(|| invalid("target_unresolved must be true or false"))
        })?;
        if map.contains_key("point") && !unresolved {
            return Err(invalid(
                "only an action whose control could not be resolved may keep a screen point",
            ));
        }
        if let Some(point) = map.get("point") {
            let point_map = object(point, "action.point", &["x", "y"])?;
            if !point_map.values().all(Value::is_number) {
                return Err(invalid("action.point must be numbers"));
            }
        }
        if let Some(key) = optional_string(map, "key", what)? {
            within(key, 64, "action.key")?;
            findings.text(&format!("/actions/{index}/key"), key);
        }
        let secure = map.get("secure").map_or(Ok(false), |value| {
            value
                .as_bool()
                .ok_or_else(|| invalid("secure must be true or false"))
        })?;
        let text = optional_string(map, "text", what)?;
        if let Some(text) = text {
            within(text, config.action_text_max_chars, "action.text")?;
        }
        let field = FieldAttrs {
            role: attrs[0],
            subrole: attrs[1],
            title: attrs[2],
            identifier: attrs[3],
            description: attrs[4],
        };
        let pointer = format!("/actions/{index}/text");
        match (field_rule(&field), text) {
            (Some(FieldRule::Secure), Some(text)) if !text.is_empty() => {
                findings.category(&pointer, Category::PasswordField);
            }
            (Some(FieldRule::Labelled(category)), Some(text)) if !text.is_empty() => {
                if text != category.marker() {
                    findings.category(&pointer, category);
                }
            }
            (_, Some(text)) => findings.text(&pointer, text),
            (_, None) => {}
        }
        if secure && text.is_some_and(|text| !text.is_empty()) {
            findings.category(&pointer, Category::PasswordField);
        }

        let mut projected = Map::new();
        projected.insert("seq".to_string(), json!(seq));
        projected.insert("at".to_string(), json!(at.to_rfc3339()));
        projected.insert("kind".to_string(), json!(kind));
        projected.insert(
            "app".to_string(),
            json!({ "bundle_id": bundle_id, "name": name }),
        );
        if !target_projection.is_null() {
            projected.insert("target".to_string(), target_projection);
        }
        if unresolved {
            projected.insert("target_unresolved".to_string(), json!(true));
        }
        if let Some(title) = window_title {
            projected.insert("window_title".to_string(), json!(title));
        }
        if let Some(text) = text {
            projected.insert("text".to_string(), json!(text));
        }
        if let Some(key) = optional_string(map, "key", what)? {
            projected.insert("key".to_string(), json!(key));
        }
        if secure {
            projected.insert("secure".to_string(), json!(true));
        }
        projected_actions.push(Value::Object(projected));
    }

    // Screenshots: bounded, spaced, de-duplicated, honestly sized; only their recognised text goes on.
    let screenshots = root
        .get("screenshots")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("archive.screenshots must be a list"))?;
    let shot_limits = config.screenshots;
    let allowed = if shot_limits.enabled {
        shot_limits.max_count
    } else {
        0
    };
    if screenshots.len() > allowed {
        return Err(bounds(format!(
            "{} screenshots; the limit is {allowed}",
            screenshots.len()
        )));
    }
    let mut screen_text = Vec::new();
    let mut previous: Option<(DateTime<Utc>, String)> = None;
    for (index, entry) in screenshots.iter().enumerate() {
        let what = "archive.screenshots[]";
        let map = object(
            entry,
            what,
            &[
                "seq",
                "at",
                "sha256",
                "bytes",
                "media_type",
                "width",
                "height",
                "data_base64",
                "screen_text",
            ],
        )?;
        let at = timestamp(map, "at", what)?;
        let sha = string(map, "sha256", what)?.to_string();
        if string(map, "media_type", what)? != "image/png" {
            return Err(invalid("a screenshot is image/png"));
        }
        let data = STANDARD
            .decode(string(map, "data_base64", what)?)
            .map_err(|_| invalid("a screenshot's data is not base64"))?;
        if data.len() > shot_limits.max_bytes {
            return Err(bounds(format!(
                "a screenshot is {} bytes; the limit is {}",
                data.len(),
                shot_limits.max_bytes
            )));
        }
        if map.get("bytes").and_then(Value::as_u64) != Some(data.len() as u64) {
            return Err(invalid("a screenshot's size does not match its bytes"));
        }
        if hex::encode(Sha256::digest(&data)) != sha {
            return Err(invalid("a screenshot's digest does not match its bytes"));
        }
        for dimension in ["width", "height"] {
            let value = map
                .get(dimension)
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid(format!("a screenshot needs a {dimension}")))?;
            if value == 0 || value > u64::from(shot_limits.max_dimension_px) {
                return Err(bounds(format!(
                    "a screenshot's {dimension} is outside 1..={}",
                    shot_limits.max_dimension_px
                )));
            }
        }
        if let Some((last_at, last_sha)) = &previous {
            let gap = (at - *last_at).num_milliseconds();
            if gap < i64::try_from(shot_limits.min_interval_ms).unwrap_or(i64::MAX) {
                return Err(bounds(format!(
                    "screenshots {gap} ms apart; the minimum is {} ms",
                    shot_limits.min_interval_ms
                )));
            }
            if *last_sha == sha {
                return Err(bounds(
                    "two consecutive screenshots are identical".to_string(),
                ));
            }
        }
        previous = Some((at, sha));
        if let Some(text) = optional_string(map, "screen_text", what)? {
            within(
                text,
                shot_limits.screen_text_max_chars,
                "screenshot.screen_text",
            )?;
            findings.text(&format!("/screenshots/{index}/screen_text"), text);
            if !text.is_empty() {
                screen_text.push(json!({ "at": at.to_rfc3339(), "text": text }));
            }
        }
    }

    // Narration.
    let transcript = root
        .get("transcript")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("archive.transcript must be a list"))?;
    if transcript.len() > config.transcript_max_segments {
        return Err(bounds(format!(
            "{} transcript segments; the limit is {}",
            transcript.len(),
            config.transcript_max_segments
        )));
    }
    let mut transcript_chars = 0usize;
    let mut projected_transcript = Vec::with_capacity(transcript.len());
    for (index, entry) in transcript.iter().enumerate() {
        let map = object(entry, "archive.transcript[]", &["at", "text"])?;
        let at = timestamp(map, "at", "transcript")?;
        let text = string(map, "text", "transcript")?;
        transcript_chars += text.chars().count();
        findings.text(&format!("/transcript/{index}/text"), text);
        projected_transcript.push(json!({ "at": at.to_rfc3339(), "text": text }));
    }
    if transcript_chars > config.transcript_max_chars {
        return Err(bounds(format!(
            "{transcript_chars} transcript characters; the limit is {}",
            config.transcript_max_chars
        )));
    }

    // Pauses.
    let mut paused_ms = 0i64;
    let mut projected_paused = Vec::new();
    for entry in root
        .get("paused")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("archive.paused must be a list"))?
    {
        let map = object(entry, "archive.paused[]", &["from", "to", "reason"])?;
        let from = timestamp(map, "from", "paused")?;
        let to = timestamp(map, "to", "paused")?;
        if to < from {
            return Err(invalid("a pause ends before it starts"));
        }
        let reason = string(map, "reason", "paused")?;
        if !PAUSE_REASONS.contains(&reason) {
            return Err(invalid(format!("`{reason}` is not a pause reason")));
        }
        paused_ms += (to - from).num_milliseconds();
        projected_paused
            .push(json!({ "from": from.to_rfc3339(), "to": to.to_rfc3339(), "reason": reason }));
    }

    // The recorder's own redaction summary: closed, counts only.
    let redaction = object(
        root.get("redaction").unwrap_or(&Value::Null),
        "archive.redaction",
        &["rule_set", "counts", "screenshots_withheld"],
    )?;
    if string(redaction, "rule_set", "redaction")? != RULE_SET
        || RULE_SET != config_rule_set(config)
    {
        return Err(invalid(format!(
            "the archive was redacted under another rule set; this server applies {RULE_SET}"
        )));
    }
    let counts = object(
        redaction.get("counts").unwrap_or(&Value::Null),
        "redaction.counts",
        &[
            "api_key",
            "bearer_token",
            "card_number",
            "verification_code",
            "password_field",
        ],
    )?;
    let mut declared = Map::new();
    for category in Category::ALL {
        let n = counts
            .get(category.as_str())
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                invalid(format!(
                    "redaction.counts.{} must be a count",
                    category.as_str()
                ))
            })?;
        declared.insert(category.as_str().to_string(), json!(n));
    }
    let withheld = redaction
        .get("screenshots_withheld")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("redaction.screenshots_withheld must be a count"))?;

    if !findings.pointers.is_empty() || !findings.categories.is_empty() {
        return Err(RecordingError::Unredacted {
            categories: findings
                .categories
                .iter()
                .map(|category| category.as_str().to_string())
                .collect(),
            pointers: findings.pointers,
        });
    }

    Ok(ArchiveReport {
        redaction: json!({
            "rule_set": RULE_SET,
            "counts": Value::Object(declared.clone()),
            "screenshots_withheld": withheld,
        }),
        stats: json!({
            "actions": actions.len(),
            "screenshots": screenshots.len(),
            "transcript_segments": transcript.len(),
            "paused_ms": paused_ms,
        }),
        projection: json!({
            "apps": apps,
            "actions": projected_actions,
            "transcript": projected_transcript,
            "screen_text": screen_text,
            "paused": projected_paused,
            "redaction": { "rule_set": RULE_SET, "counts": Value::Object(declared) },
        }),
    })
}

fn config_rule_set(_config: &RecordingConfig) -> &'static str {
    // `RecordingConfig::parse` refuses any rule set but this server's own.
    RULE_SET
}

#[cfg(test)]
mod tests {
    use super::*;

    const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP003";
    const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP003";
    const RECORDING: &str = "rec_01J8Z3K6F1N8VQ2X5W9YADP003";

    pub(crate) fn archive() -> Value {
        json!({
            "schema_version": "v1",
            "recording_id": RECORDING,
            "owner": { "tenant_id": TENANT, "user_id": USER, "epoch": 3 },
            "platform": "macos",
            "started_at": "2026-10-04T10:00:00Z",
            "finished_at": "2026-10-04T10:05:00Z",
            "finish": { "by": "user", "request_id": "fin_1" },
            "apps": [{ "bundle_id": "com.apple.Notes", "name": "Notes" }],
            "actions": [
                {
                    "seq": 1, "at": "2026-10-04T10:00:05Z", "kind": "click",
                    "app": { "bundle_id": "com.apple.Notes", "name": "Notes" },
                    "target": { "ax_role": "AXButton", "ax_title": "New Note" }
                },
                {
                    "seq": 2, "at": "2026-10-04T10:00:09Z", "kind": "type",
                    "app": { "bundle_id": "com.apple.Notes", "name": "Notes" },
                    "target": { "ax_role": "AXTextArea", "ax_title": "Note body" },
                    "text": "Quarterly report"
                }
            ],
            "screenshots": [],
            "transcript": [{ "at": "2026-10-04T10:00:06Z", "text": "now I make a new note" }],
            "paused": [],
            "redaction": {
                "rule_set": "v1",
                "counts": { "api_key": 0, "bearer_token": 0, "card_number": 0, "verification_code": 0, "password_field": 0 },
                "screenshots_withheld": 0
            },
            "limits": {}
        })
    }

    fn expected() -> Expected<'static> {
        Expected {
            recording_id: RECORDING,
            tenant_id: TENANT,
            user_id: USER,
            epoch: 3,
        }
    }

    fn run(document: &Value) -> Result<ArchiveReport, RecordingError> {
        validate(
            document.to_string().as_bytes(),
            &expected(),
            RecordingConfig::committed(),
        )
    }

    /// The archive the macOS recorder produces for a scripted session (`native/macos`, which writes and
    /// checks this very file), accepted by the server's own validator: the two cannot drift on the format.
    #[test]
    fn the_recorders_own_archive_is_accepted() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/contract/fixtures/recording/archive_from_recorder.json");
        let bytes = std::fs::read(&path).expect("the recorder's golden archive");
        let report = validate(&bytes, &expected(), RecordingConfig::committed()).expect("valid");
        assert_eq!(report.stats["actions"], 4);
        assert_eq!(report.redaction["counts"]["password_field"], 1);
        assert!(report.stats["paused_ms"].as_i64().unwrap() >= 0);
        let projection = report.projection.to_string();
        assert!(projection.contains("New Note") && !projection.contains("hunter2"));
    }

    #[test]
    fn a_redacted_archive_is_accepted_and_projected_without_pixels_or_points() {
        let mut document = archive();
        document["actions"][0]["target_unresolved"] = json!(true);
        document["actions"][0]["point"] = json!({ "x": 10.5, "y": 20.0 });
        let report = run(&document).expect("valid");
        assert_eq!(report.stats["actions"], 2);
        let projection = report.projection.to_string();
        assert!(!projection.contains("\"point\""), "{projection}");
        assert!(!projection.contains("data_base64"));
        assert!(report.projection["actions"][0]["target_unresolved"] == true);
    }

    #[test]
    fn an_unredacted_value_names_where_and_what_but_never_the_text() {
        let mut document = archive();
        document["actions"][1]["text"] = json!("AKIAIOSFODNN7EXAMPLE");
        document["transcript"][0]["text"] = json!("my card is 4111 1111 1111 1111");
        let error = run(&document).expect_err("unredacted");
        let RecordingError::Unredacted {
            categories,
            pointers,
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!(categories, &["api_key", "card_number"]);
        assert!(pointers.contains(&"/actions/1/text".to_string()));
        assert!(pointers.contains(&"/transcript/0/text".to_string()));
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains("AKIA") && !rendered.contains("4111"),
            "{rendered}"
        );
    }

    #[test]
    fn text_in_a_secure_or_labelled_field_must_already_be_gone() {
        let mut document = archive();
        document["actions"][1]["target"] =
            json!({ "ax_role": "AXTextField", "ax_subrole": "AXSecureTextField" });
        document["actions"][1]["text"] = json!("hunter2");
        let error = run(&document).expect_err("secure text kept");
        assert!(
            matches!(error, RecordingError::Unredacted { ref categories, .. } if categories == &["password_field"])
        );
        document["actions"][1]
            .as_object_mut()
            .unwrap()
            .remove("text");
        document["actions"][1]["secure"] = json!(true);
        assert!(run(&document).is_ok(), "the dropped form is accepted");
        let mut labelled = archive();
        labelled["actions"][1]["target"] =
            json!({ "ax_role": "AXTextField", "ax_title": "Verification code" });
        labelled["actions"][1]["text"] = json!("123456");
        assert!(matches!(
            run(&labelled),
            Err(RecordingError::Unredacted { .. })
        ));
        labelled["actions"][1]["text"] = json!("[REDACTED:verification_code]");
        assert!(run(&labelled).is_ok());
    }

    #[test]
    fn another_owner_recording_or_epoch_is_refused() {
        for (path, value) in [
            ("/owner/user_id", json!("usr_01J8Z3K6F1N8VQ2X5W9YOTHER1")),
            ("/owner/tenant_id", json!("tn_01J8Z3K6F1N8VQ2X5W9YOTHER1")),
            ("/owner/epoch", json!(4)),
        ] {
            let mut document = archive();
            *document.pointer_mut(path).unwrap() = value;
            assert!(
                matches!(run(&document), Err(RecordingError::OwnerChanged { .. })),
                "{path}"
            );
        }
        let mut document = archive();
        document["recording_id"] = json!("rec_01J8Z3K6F1N8VQ2X5W9YOTHER1");
        assert!(matches!(
            run(&document),
            Err(RecordingError::InvalidArchive(_))
        ));
    }

    #[test]
    fn a_recording_the_person_did_not_finish_is_not_an_archive() {
        let mut document = archive();
        document["finish"]["by"] = json!("model");
        assert!(matches!(
            run(&document),
            Err(RecordingError::InvalidArchive(_))
        ));
        document["finish"] = Value::Null;
        assert!(matches!(
            run(&document),
            Err(RecordingError::InvalidArchive(_))
        ));
    }

    #[test]
    fn the_shape_is_closed_and_the_limits_hold() {
        let mut document = archive();
        document["actions"][0]["extra"] = json!(1);
        assert!(matches!(
            run(&document),
            Err(RecordingError::InvalidArchive(_))
        ));
        let mut document = archive();
        document["actions"][0]["point"] = json!({ "x": 1, "y": 2 });
        assert!(
            matches!(run(&document), Err(RecordingError::InvalidArchive(_))),
            "a point needs an unresolved target"
        );
        let mut document = archive();
        document["actions"][0]["kind"] = json!("teleport");
        assert!(matches!(
            run(&document),
            Err(RecordingError::InvalidArchive(_))
        ));
        let mut document = archive();
        document["actions"][1]["seq"] = json!(1);
        assert!(matches!(
            run(&document),
            Err(RecordingError::InvalidArchive(_))
        ));
        let mut document = archive();
        document["redaction"]["rule_set"] = json!("v0");
        assert!(matches!(
            run(&document),
            Err(RecordingError::InvalidArchive(_))
        ));
        let mut document = archive();
        let long = "x".repeat(RecordingConfig::committed().action_text_max_chars + 1);
        document["actions"][1]["text"] = json!(long);
        assert!(matches!(
            run(&document),
            Err(RecordingError::ArchiveBounds(_))
        ));
        let huge = vec![b'x'; RecordingConfig::committed().archive_max_bytes + 1];
        assert!(matches!(
            validate(&huge, &expected(), RecordingConfig::committed()),
            Err(RecordingError::ArchiveBounds(_))
        ));
    }

    #[test]
    fn screenshots_are_checked_against_their_own_bytes_and_each_other() {
        let png = |fill: u8| STANDARD.encode(vec![fill; 64]);
        let shot = |at: &str, fill: u8| {
            let data = vec![fill; 64];
            json!({
                "seq": 1, "at": at, "sha256": hex::encode(Sha256::digest(&data)), "bytes": 64,
                "media_type": "image/png", "width": 100, "height": 80,
                "data_base64": png(fill), "screen_text": "Notes"
            })
        };
        let mut document = archive();
        document["screenshots"] = json!([
            shot("2026-10-04T10:00:10Z", 1),
            shot("2026-10-04T10:00:20Z", 2)
        ]);
        let report = run(&document).expect("spaced and distinct");
        assert_eq!(report.stats["screenshots"], 2);
        assert_eq!(
            report.projection["screen_text"].as_array().unwrap().len(),
            2
        );

        let mut close = archive();
        close["screenshots"] = json!([
            shot("2026-10-04T10:00:10Z", 1),
            shot("2026-10-04T10:00:11Z", 2)
        ]);
        assert!(
            matches!(run(&close), Err(RecordingError::ArchiveBounds(_))),
            "min interval"
        );
        let mut same = archive();
        same["screenshots"] = json!([
            shot("2026-10-04T10:00:10Z", 1),
            shot("2026-10-04T10:00:20Z", 1)
        ]);
        assert!(
            matches!(run(&same), Err(RecordingError::ArchiveBounds(_))),
            "de-duplication"
        );
        let mut lying = archive();
        let mut first = shot("2026-10-04T10:00:10Z", 1);
        first["sha256"] = json!("0".repeat(64));
        lying["screenshots"] = json!([first]);
        assert!(matches!(
            run(&lying),
            Err(RecordingError::InvalidArchive(_))
        ));
        let mut many = archive();
        let cap = RecordingConfig::committed().screenshots.max_count;
        many["screenshots"] = Value::Array(
            (0..=cap)
                .map(|i| shot("2026-10-04T10:00:10Z", u8::try_from(i % 200).unwrap()))
                .collect(),
        );
        assert!(
            matches!(run(&many), Err(RecordingError::ArchiveBounds(_))),
            "count"
        );
        let mut text = archive();
        let mut with_key = shot("2026-10-04T10:00:10Z", 1);
        with_key["screen_text"] = json!("token ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        text["screenshots"] = json!([with_key]);
        assert!(
            matches!(run(&text), Err(RecordingError::Unredacted { .. })),
            "screen text is scanned"
        );
    }
}
