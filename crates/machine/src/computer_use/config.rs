//! `config/computer-use.yaml`: the helper pin, the sequence limits and the guide panes (ADP-002).
//!
//! Policy is configuration, never a source constant (D-018). The same file is validated against
//! `schemas/json/computer-use-config.schema.json` by the contract tests; this loader repeats the checks
//! that matter to safety so a copy of the file edited on a host without the schema tooling still fails
//! closed: an unknown key, a missing section, a published helper with no team id and a switched-off Apple
//! anchor are all load errors, never defaults.

use std::sync::OnceLock;
use std::time::Duration;

use serde_yaml::Value;

use super::permission::Permission;

/// The committed configuration.
pub const COMPUTER_USE_YAML: &str = include_str!("../../../../config/computer-use.yaml");

/// A configuration that cannot be trusted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("computer-use configuration is invalid: {0}")]
pub struct ConfigError(pub String);

/// Whether a signing identity exists for the helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperStatus {
    /// A team id is configured and a helper signed by it may be launched.
    Published,
    /// No signing identity has been issued; no helper may be launched.
    Unpublished,
}

/// The helper's pinned identity and where it is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperConfig {
    /// Whether a signing identity exists.
    pub status: HelperStatus,
    /// Code-signing identifiers a helper may carry.
    pub allowed_bundle_ids: Vec<String>,
    /// The Apple Developer Team ID that must have signed it; present once published.
    pub team_id: Option<String>,
    /// Directory under the application data directory holding versions and pointers.
    pub install_directory: String,
    /// Versions kept on disk (the active one and its rollback target are always kept).
    pub keep_versions: usize,
    /// Prefix of the `--profile` value every helper this runtime launches carries.
    pub profile_prefix: String,
    /// How long a terminated orphan has to exit before it is killed.
    pub terminate_grace: Duration,
}

/// How many actions, how long, how much text one sequence may spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceLimits {
    /// Actions one ToolCall may carry out.
    pub max_actions: usize,
    /// Wall time from the first action to the last.
    pub max_duration: Duration,
    /// Bytes typed or written to the clipboard across the sequence.
    pub max_typed_bytes: usize,
}

/// The Privacy pane anchor each permission's guide deep-links to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionPanes {
    accessibility: String,
    screen_recording: String,
    input_monitoring: String,
    microphone: String,
}

impl PermissionPanes {
    /// The pane anchor for `permission`.
    #[must_use]
    pub fn pane(&self, permission: Permission) -> &str {
        match permission {
            Permission::Accessibility => &self.accessibility,
            Permission::ScreenRecording => &self.screen_recording,
            Permission::InputMonitoring => &self.input_monitoring,
            Permission::Microphone => &self.microphone,
        }
    }
}

/// The whole configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerUseConfig {
    /// The helper pin.
    pub helper: HelperConfig,
    /// Sequence limits.
    pub limits: SequenceLimits,
    /// Settings panes.
    pub panes: PermissionPanes,
    /// How often the desktop polls the permission status.
    pub poll_interval: Duration,
}

impl ComputerUseConfig {
    /// The committed configuration, parsed once.
    ///
    /// # Panics
    /// Panics if the committed `config/computer-use.yaml` does not satisfy [`Self::parse`]; the
    /// repository's own tests prove it does.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static BUILTIN: OnceLock<ComputerUseConfig> = OnceLock::new();
        BUILTIN.get_or_init(|| {
            Self::parse(COMPUTER_USE_YAML)
                .expect("config/computer-use.yaml is a valid configuration")
        })
    }

    /// Parse and validate a configuration document.
    ///
    /// # Errors
    /// Returns [`ConfigError`] for malformed YAML, an unknown or missing key, an out-of-range number, an
    /// Apple anchor or hardened runtime switched off, or a published helper without a team id.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let root: Value =
            serde_yaml::from_str(source).map_err(|error| ConfigError(error.to_string()))?;
        let root = mapping(&root, "the document")?;
        only_keys(
            root,
            "the document",
            &["version", "helper", "sequence_limits", "permission_guide"],
        )?;
        if get(root, "version")?.as_u64() != Some(1) {
            return Err(ConfigError("version must be 1".into()));
        }
        let helper = Self::parse_helper(get(root, "helper")?)?;
        let limits = Self::parse_limits(get(root, "sequence_limits")?)?;
        let (panes, poll_interval) = Self::parse_guide(get(root, "permission_guide")?)?;
        Ok(Self {
            helper,
            limits,
            panes,
            poll_interval,
        })
    }

    fn parse_helper(value: &Value) -> Result<HelperConfig, ConfigError> {
        let helper = mapping(value, "helper")?;
        only_keys(
            helper,
            "helper",
            &[
                "status",
                "allowed_bundle_ids",
                "team_id",
                "require_apple_anchor",
                "require_hardened_runtime",
                "install",
                "orphan_sweep",
                "blocker",
            ],
        )?;
        let status = match text(get(helper, "status")?, "helper.status")? {
            "published" => HelperStatus::Published,
            "unpublished" => HelperStatus::Unpublished,
            other => return Err(ConfigError(format!("helper.status {other:?} is unknown"))),
        };
        // The anchor and the hardened runtime are requirements, not options: the file may state them but
        // it cannot turn them off.
        for key in ["require_apple_anchor", "require_hardened_runtime"] {
            if get(helper, key)?.as_bool() != Some(true) {
                return Err(ConfigError(format!("helper.{key} must be true")));
            }
        }
        let allowed_bundle_ids = get(helper, "allowed_bundle_ids")?
            .as_sequence()
            .ok_or_else(|| ConfigError("helper.allowed_bundle_ids must be a list".into()))?
            .iter()
            .map(|id| text(id, "helper.allowed_bundle_ids[]").and_then(identifier))
            .collect::<Result<Vec<_>, _>>()?;
        if allowed_bundle_ids.is_empty() {
            return Err(ConfigError("helper.allowed_bundle_ids is empty".into()));
        }
        let team_id = match get(helper, "team_id")? {
            Value::Null => None,
            other => Some(team_id(text(other, "helper.team_id")?)?),
        };
        if status == HelperStatus::Published && team_id.is_none() {
            return Err(ConfigError(
                "a published helper must name its team_id".into(),
            ));
        }
        let install = mapping(get(helper, "install")?, "helper.install")?;
        only_keys(install, "helper.install", &["directory", "keep_versions"])?;
        let directory = text(get(install, "directory")?, "helper.install.directory")?;
        if directory.is_empty()
            || !directory
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ConfigError(
                "helper.install.directory must be one lowercase path segment".into(),
            ));
        }
        let keep_versions = bounded(
            get(install, "keep_versions")?,
            "helper.install.keep_versions",
            2,
            8,
        )?;
        let sweep = mapping(get(helper, "orphan_sweep")?, "helper.orphan_sweep")?;
        only_keys(
            sweep,
            "helper.orphan_sweep",
            &["profile_prefix", "terminate_grace_ms"],
        )?;
        let profile_prefix = text(
            get(sweep, "profile_prefix")?,
            "helper.orphan_sweep.profile_prefix",
        )?;
        if profile_prefix.len() < 4
            || !profile_prefix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ConfigError(
                "helper.orphan_sweep.profile_prefix must be at least four lowercase characters"
                    .into(),
            ));
        }
        let grace = bounded(
            get(sweep, "terminate_grace_ms")?,
            "helper.orphan_sweep.terminate_grace_ms",
            100,
            60_000,
        )?;
        Ok(HelperConfig {
            status,
            allowed_bundle_ids,
            team_id,
            install_directory: directory.to_string(),
            keep_versions,
            profile_prefix: profile_prefix.to_string(),
            terminate_grace: Duration::from_millis(grace as u64),
        })
    }

    fn parse_limits(value: &Value) -> Result<SequenceLimits, ConfigError> {
        let limits = mapping(value, "sequence_limits")?;
        only_keys(
            limits,
            "sequence_limits",
            &["max_actions", "max_duration_ms", "max_typed_bytes"],
        )?;
        Ok(SequenceLimits {
            max_actions: bounded(
                get(limits, "max_actions")?,
                "sequence_limits.max_actions",
                1,
                256,
            )?,
            max_duration: Duration::from_millis(bounded(
                get(limits, "max_duration_ms")?,
                "sequence_limits.max_duration_ms",
                1000,
                3_600_000,
            )? as u64),
            max_typed_bytes: bounded(
                get(limits, "max_typed_bytes")?,
                "sequence_limits.max_typed_bytes",
                1,
                1_048_576,
            )?,
        })
    }

    fn parse_guide(value: &Value) -> Result<(PermissionPanes, Duration), ConfigError> {
        let guide = mapping(value, "permission_guide")?;
        only_keys(guide, "permission_guide", &["panes", "poll_interval_ms"])?;
        let panes = mapping(get(guide, "panes")?, "permission_guide.panes")?;
        only_keys(
            panes,
            "permission_guide.panes",
            &[
                "accessibility",
                "screen_recording",
                "input_monitoring",
                "microphone",
            ],
        )?;
        let pane = |key: &str| -> Result<String, ConfigError> {
            let name = text(get(panes, key)?, key)?;
            // A pane is an anchor in the Privacy & Security settings, nothing a deep link could widen.
            if !name.starts_with("Privacy_")
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(ConfigError(format!(
                    "permission_guide.panes.{key} {name:?} is not a Privacy pane anchor"
                )));
            }
            Ok(name.to_string())
        };
        let poll = bounded(
            get(guide, "poll_interval_ms")?,
            "permission_guide.poll_interval_ms",
            500,
            60_000,
        )?;
        Ok((
            PermissionPanes {
                accessibility: pane("accessibility")?,
                screen_recording: pane("screen_recording")?,
                input_monitoring: pane("input_monitoring")?,
                microphone: pane("microphone")?,
            },
            Duration::from_millis(poll as u64),
        ))
    }
}

type Map = serde_yaml::Mapping;

fn mapping<'a>(value: &'a Value, what: &str) -> Result<&'a Map, ConfigError> {
    value
        .as_mapping()
        .ok_or_else(|| ConfigError(format!("{what} must be a mapping")))
}

fn get<'a>(map: &'a Map, key: &str) -> Result<&'a Value, ConfigError> {
    map.get(key)
        .ok_or_else(|| ConfigError(format!("{key} is required")))
}

fn only_keys(map: &Map, what: &str, allowed: &[&str]) -> Result<(), ConfigError> {
    for key in map.keys() {
        match key.as_str() {
            Some(name) if allowed.contains(&name) => {}
            other => {
                return Err(ConfigError(format!("{what} has an unknown key {other:?}")));
            }
        }
    }
    Ok(())
}

fn text<'a>(value: &'a Value, what: &str) -> Result<&'a str, ConfigError> {
    value
        .as_str()
        .ok_or_else(|| ConfigError(format!("{what} must be a string")))
}

fn bounded(value: &Value, what: &str, min: usize, max: usize) -> Result<usize, ConfigError> {
    let number = value
        .as_u64()
        .ok_or_else(|| ConfigError(format!("{what} must be a non-negative integer")))?;
    let number = usize::try_from(number)
        .map_err(|_| ConfigError(format!("{what} does not fit this platform")))?;
    if (min..=max).contains(&number) {
        Ok(number)
    } else {
        Err(ConfigError(format!(
            "{what} must be between {min} and {max}"
        )))
    }
}

fn identifier(value: &str) -> Result<String, ConfigError> {
    let valid = value.contains('.')
        && value.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if valid {
        Ok(value.to_string())
    } else {
        Err(ConfigError(format!("{value:?} is not a bundle identifier")))
    }
}

fn team_id(value: &str) -> Result<String, ConfigError> {
    if value.len() == 10
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        Ok(value.to_string())
    } else {
        Err(ConfigError(format!("{value:?} is not an Apple Team ID")))
    }
}
