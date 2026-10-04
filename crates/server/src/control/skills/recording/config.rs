//! `config/skill-recording.yaml`: capture limits, the archive cap, the lifetime of an unfinished recording
//! and the tool-path preferences (ADP-003, D-018).
//!
//! The same file is validated against `schemas/json/skill-recording-config.schema.json` by the contract
//! tests; this loader repeats the checks that matter to safety so a copy edited on a host without the
//! schema tooling still fails closed: an unknown key, a missing section, a zero or absurd limit and a rule
//! set other than the one this server implements are load errors, never defaults.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};

use super::redact::RULE_SET;

/// The committed configuration.
pub const SKILL_RECORDING_YAML: &str = include_str!("../../../../../../config/skill-recording.yaml");

/// A configuration that cannot be trusted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("skill-recording configuration is invalid: {0}")]
pub struct ConfigError(pub String);

/// The screenshot limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenshotLimits {
    /// Whether the recorder may take screenshots at all.
    pub enabled: bool,
    /// Most screenshots one archive may hold.
    pub max_count: usize,
    /// Least time between two kept screenshots.
    pub min_interval_ms: u64,
    /// Most bytes one screenshot may hold.
    pub max_bytes: usize,
    /// Longest side, in pixels.
    pub max_dimension_px: u32,
    /// Most characters of recognised screen text kept per screenshot.
    pub screen_text_max_chars: usize,
}

/// One tool-path preference: applications whose workflow has an equivalent Tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPath {
    /// A stable name.
    pub id: String,
    /// The applications (bundle ids) this path applies to.
    pub bundle_ids: Vec<String>,
    /// Concrete tools, when the path names them.
    pub tools: Vec<String>,
    /// A family prefix (`connector.slack.`), when the path names one.
    pub tool_prefix: Option<String>,
}

/// The recording configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingConfig {
    /// How long an unfinished recording may last.
    pub max_duration: Duration,
    /// Most actions in one archive.
    pub max_actions: usize,
    /// Most characters of typed or entered text per action.
    pub action_text_max_chars: usize,
    /// Most characters of any window or control attribute.
    pub attribute_max_chars: usize,
    /// Screenshot limits.
    pub screenshots: ScreenshotLimits,
    /// Most transcript segments.
    pub transcript_max_segments: usize,
    /// Most transcript characters in total.
    pub transcript_max_chars: usize,
    /// Most bytes in one archive.
    pub archive_max_bytes: usize,
    /// Most bytes in one stored proposal.
    pub proposal_max_bytes: usize,
    /// Most steps in one proposal.
    pub proposal_max_steps: usize,
    /// Most slots in one proposal.
    pub proposal_max_slots: usize,
    /// Tool-path preferences.
    pub tool_paths: Vec<ToolPath>,
    raw: Value,
}

fn get<'a>(value: &'a serde_yaml::Value, key: &str) -> Result<&'a serde_yaml::Value, ConfigError> {
    value
        .get(key)
        .ok_or_else(|| ConfigError(format!("missing `{key}`")))
}

fn uint(value: &serde_yaml::Value, key: &str, min: u64, max: u64) -> Result<u64, ConfigError> {
    let found = get(value, key)?
        .as_u64()
        .ok_or_else(|| ConfigError(format!("`{key}` must be a whole number")))?;
    if (min..=max).contains(&found) {
        Ok(found)
    } else {
        Err(ConfigError(format!("`{key}` must be {min}..={max}, found {found}")))
    }
}

fn closed(value: &serde_yaml::Value, what: &str, keys: &[&str]) -> Result<(), ConfigError> {
    let map = value
        .as_mapping()
        .ok_or_else(|| ConfigError(format!("`{what}` must be a mapping")))?;
    for key in map.keys() {
        let key = key.as_str().unwrap_or("?");
        if !keys.contains(&key) {
            return Err(ConfigError(format!("`{what}` has an unknown key `{key}`")));
        }
    }
    Ok(())
}

fn usize_of(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl RecordingConfig {
    /// Parse and check a configuration document.
    ///
    /// # Errors
    /// [`ConfigError`] for anything the schema would refuse.
    pub fn parse(yaml: &str) -> Result<Self, ConfigError> {
        let document: serde_yaml::Value =
            serde_yaml::from_str(yaml).map_err(|error| ConfigError(error.to_string()))?;
        closed(
            &document,
            "document",
            &["version", "rule_set", "max_duration_ms", "limits", "proposal", "tool_paths"],
        )?;
        if get(&document, "version")?.as_u64() != Some(1) {
            return Err(ConfigError("`version` must be 1".to_string()));
        }
        if get(&document, "rule_set")?.as_str() != Some(RULE_SET) {
            return Err(ConfigError(format!(
                "`rule_set` must be {RULE_SET:?}: this server implements no other"
            )));
        }
        let limits = get(&document, "limits")?;
        closed(
            limits,
            "limits",
            &[
                "max_actions",
                "action_text_max_chars",
                "attribute_max_chars",
                "screenshots",
                "transcript",
                "archive",
            ],
        )?;
        let shots = get(limits, "screenshots")?;
        closed(
            shots,
            "limits.screenshots",
            &[
                "enabled",
                "max_count",
                "min_interval_ms",
                "max_bytes",
                "max_dimension_px",
                "screen_text_max_chars",
            ],
        )?;
        let transcript = get(limits, "transcript")?;
        closed(transcript, "limits.transcript", &["max_segments", "max_chars"])?;
        let archive = get(limits, "archive")?;
        closed(archive, "limits.archive", &["max_bytes"])?;
        let proposal = get(&document, "proposal")?;
        closed(proposal, "proposal", &["max_bytes", "max_steps", "max_slots"])?;
        let mut tool_paths = Vec::new();
        for entry in get(&document, "tool_paths")?
            .as_sequence()
            .ok_or_else(|| ConfigError("`tool_paths` must be a list".to_string()))?
        {
            closed(entry, "tool_paths[]", &["id", "bundle_ids", "tools", "tool_prefix"])?;
            let strings = |key: &str| -> Vec<String> {
                entry
                    .get(key)
                    .and_then(serde_yaml::Value::as_sequence)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let path = ToolPath {
                id: get(entry, "id")?.as_str().unwrap_or_default().to_string(),
                bundle_ids: strings("bundle_ids"),
                tools: strings("tools"),
                tool_prefix: entry
                    .get("tool_prefix")
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::to_string),
            };
            if path.id.is_empty() || path.bundle_ids.is_empty() {
                return Err(ConfigError("a tool path needs an id and bundle ids".to_string()));
            }
            if path.tools.is_empty() == path.tool_prefix.is_none() {
                return Err(ConfigError(format!(
                    "tool path `{}` names either tools or a tool_prefix",
                    path.id
                )));
            }
            tool_paths.push(path);
        }
        let config = Self {
            max_duration: Duration::from_millis(uint(&document, "max_duration_ms", 60_000, 7_200_000)?),
            max_actions: usize_of(uint(limits, "max_actions", 1, 10_000)?),
            action_text_max_chars: usize_of(uint(limits, "action_text_max_chars", 1, 10_000)?),
            attribute_max_chars: usize_of(uint(limits, "attribute_max_chars", 16, 1_000)?),
            screenshots: ScreenshotLimits {
                enabled: get(shots, "enabled")?
                    .as_bool()
                    .ok_or_else(|| ConfigError("`enabled` must be true or false".to_string()))?,
                max_count: usize_of(uint(shots, "max_count", 0, 100)?),
                min_interval_ms: uint(shots, "min_interval_ms", 1_000, 600_000)?,
                max_bytes: usize_of(uint(shots, "max_bytes", 1_024, 1_048_576)?),
                max_dimension_px: u32::try_from(uint(shots, "max_dimension_px", 256, 4_096)?)
                    .unwrap_or(4_096),
                screen_text_max_chars: usize_of(uint(shots, "screen_text_max_chars", 0, 10_000)?),
            },
            transcript_max_segments: usize_of(uint(transcript, "max_segments", 0, 5_000)?),
            transcript_max_chars: usize_of(uint(transcript, "max_chars", 0, 200_000)?),
            archive_max_bytes: usize_of(uint(archive, "max_bytes", 1_024, 8_388_608)?),
            proposal_max_bytes: usize_of(uint(proposal, "max_bytes", 1_024, 1_048_576)?),
            proposal_max_steps: usize_of(uint(proposal, "max_steps", 1, 1_000)?),
            proposal_max_slots: usize_of(uint(proposal, "max_slots", 1, 256)?),
            tool_paths,
            raw: serde_json::to_value(&document).map_err(|error| ConfigError(error.to_string()))?,
        };
        Ok(config)
    }

    /// The committed configuration, parsed once.
    ///
    /// # Panics
    /// When the committed file is invalid; the contract tests catch that before a release.
    #[must_use]
    pub fn committed() -> &'static Self {
        static COMMITTED: OnceLock<RecordingConfig> = OnceLock::new();
        COMMITTED.get_or_init(|| {
            Self::parse(SKILL_RECORDING_YAML).unwrap_or_else(|error| panic!("{error}"))
        })
    }

    /// The limits the recorder must enforce, as `StartRecording` returns them.
    #[must_use]
    pub fn limits_json(&self) -> Value {
        self.raw["limits"].clone()
    }

    /// What the compiler needs of the configuration: the limits and the tool paths.
    #[must_use]
    pub fn settings_json(&self) -> Value {
        json!({
            "limits": self.raw["limits"],
            "proposal": self.raw["proposal"],
            "tool_paths": self.raw["tool_paths"],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_file_loads() {
        let config = RecordingConfig::committed();
        assert!(config.archive_max_bytes <= 8 * 1024 * 1024);
        assert!(config.screenshots.max_count > 0);
        assert!(config.tool_paths.iter().any(|path| path.id == "managed_browser"));
        assert_eq!(config.limits_json()["archive"]["max_bytes"], config.archive_max_bytes);
    }

    #[test]
    fn a_weakened_or_unknown_configuration_is_refused() {
        let broken = |from: &str, to: &str| SKILL_RECORDING_YAML.replacen(from, to, 1);
        for (from, to) in [
            ("rule_set: v1", "rule_set: v0"),
            ("max_bytes: 8388608", "max_bytes: 99999999"),
            ("max_count: 24", "max_count: 100000"),
            ("min_interval_ms: 5000", "min_interval_ms: 1"),
            ("max_actions: 2000", "max_actions: 0"),
            ("limits:\n", "limits:\n  unbounded: true\n"),
            ("version: 1", "version: 2"),
        ] {
            assert!(
                RecordingConfig::parse(&broken(from, to)).is_err(),
                "{from} -> {to} must be refused"
            );
        }
    }
}
