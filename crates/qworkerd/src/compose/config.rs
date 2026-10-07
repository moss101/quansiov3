//! `config/composition.yaml` (ADP-009, DOMAIN.md §7.5 "Composition sandbox"): the sandbox's limits and the host-call error
//! codes that end a cell. Policy lives in the file, never in a source constant (D-018); the JSON Schema
//! `schemas/json/composition-config.schema.json` bounds every value and is checked by the repository gates.

use std::collections::BTreeSet;

use serde::Deserialize;

const SHIPPED: &str = include_str!("../../../../config/composition.yaml");

/// The sandbox's limits.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeLimits {
    /// The interpreter's heap.
    pub memory_bytes: usize,
    /// The interpreter's native stack.
    pub stack_bytes: usize,
    /// Default wall-clock time of one cell.
    pub deadline_ms: u64,
    /// The most a caller may ask for.
    pub deadline_max_ms: u64,
    /// Default host calls one cell may make.
    pub host_call_budget: u32,
    /// The most a caller may ask for.
    pub host_call_budget_max: u32,
    /// Bytes of the script.
    pub max_script_bytes: usize,
    /// Bytes of one call's arguments.
    pub max_args_bytes: usize,
    /// Bytes of one call's result as handed to the script.
    pub max_host_result_bytes: usize,
    /// Bytes of text a cell returns.
    pub output_text_bytes: usize,
    /// Image references a cell returns.
    pub output_images: usize,
    /// Bytes of one image reference.
    pub image_ref_bytes: usize,
    /// Bytes of persisted `state`.
    pub state_max_bytes: usize,
    /// Idle time after which a session's state is dropped.
    pub idle_timeout_ms: u64,
    /// Sessions held at once.
    pub max_sessions: usize,
}

/// `config/composition.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeConfig {
    /// Always 1.
    pub version: u32,
    /// The master switch.
    pub enabled: bool,
    /// Bounds.
    pub limits: ComposeLimits,
    /// Host-call error codes that end a cell.
    pub terminal_codes: Vec<String>,
}

/// A configuration that breaks a rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("config/composition.yaml: {0}")]
pub struct ConfigError(pub String);

impl ComposeConfig {
    /// The shipped configuration.
    ///
    /// # Panics
    /// Panics only if the repository's own `config/composition.yaml` is invalid, which its tests catch first.
    #[must_use]
    pub fn builtin() -> Self {
        Self::from_yaml(SHIPPED)
            .expect("config/composition.yaml is a valid composition sandbox configuration")
    }

    /// Parse and validate a configuration.
    ///
    /// # Errors
    /// Returns [`ConfigError`] for YAML that does not parse and for the first rule it breaks.
    pub fn from_yaml(source: &str) -> Result<Self, ConfigError> {
        let config: Self =
            serde_yaml::from_str(source).map_err(|error| ConfigError(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Check the rules a JSON Schema states per field and the ones it can not state across fields.
    ///
    /// # Errors
    /// Returns [`ConfigError`] naming the first rule it breaks.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |detail: &str| Err(ConfigError(detail.to_string()));
        let limits = &self.limits;
        if self.version != 1 {
            return bad("version must be 1");
        }
        if limits.deadline_ms > limits.deadline_max_ms || limits.deadline_max_ms > 120_000 {
            return bad("deadline_ms must not pass deadline_max_ms, which is at most 120000");
        }
        if limits.host_call_budget > limits.host_call_budget_max
            || limits.host_call_budget_max > 512
        {
            return bad(
                "host_call_budget must not pass host_call_budget_max, which is at most 512",
            );
        }
        if limits.memory_bytes < 1_048_576 || limits.stack_bytes < 65_536 {
            return bad("the interpreter needs at least 1 MiB of heap and 64 KiB of stack");
        }
        let codes: BTreeSet<&str> = self.terminal_codes.iter().map(String::as_str).collect();
        if codes.len() != self.terminal_codes.len() {
            return bad("terminal_codes lists a code twice");
        }
        for required in [
            "COMPUTER_TAKEOVER_ACTIVE",
            "COMPUTER_PERMISSION_REVOKED",
            "COMPUTER_STOPPED_BY_USER",
            "BUDGET_EXHAUSTED",
            "BROWSER_INPUT_FENCED",
        ] {
            if !codes.contains(required) {
                return Err(ConfigError(format!(
                    "terminal_codes must keep {required}: a takeover, a revoked permission, a stop and a spent budget always end a cell"
                )));
            }
        }
        Ok(())
    }

    /// Whether a host error code ends the cell.
    #[must_use]
    pub fn is_terminal(&self, code: &str) -> bool {
        self.terminal_codes.iter().any(|terminal| terminal == code)
    }
}

impl ComposeLimits {
    /// The deadline a cell gets: what it asked for, never above the ceiling and never zero.
    #[must_use]
    pub fn deadline_for(&self, asked_ms: Option<u64>) -> u64 {
        asked_ms
            .unwrap_or(self.deadline_ms)
            .clamp(1, self.deadline_max_ms)
    }

    /// The host-call budget a cell gets: what it asked for, never above the ceiling and never zero.
    #[must_use]
    pub fn budget_for(&self, asked: Option<u32>) -> u32 {
        asked
            .unwrap_or(self.host_call_budget)
            .clamp(1, self.host_call_budget_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_configuration_is_valid_and_keeps_the_four_mandatory_terminal_conditions() {
        let config = ComposeConfig::builtin();
        assert!(config.is_terminal("COMPUTER_TAKEOVER_ACTIVE"));
        assert!(config.is_terminal("BUDGET_EXHAUSTED"));
        assert!(
            !config.is_terminal("BROWSER_STALE_PAGE"),
            "a stale page is the script's to handle"
        );
        assert_eq!(config.limits.memory_bytes, 64 * 1024 * 1024);
        assert_eq!(config.limits.deadline_ms, 30_000);
        assert_eq!(config.limits.host_call_budget, 64);
        assert_eq!(config.limits.output_text_bytes, 38 * 1024);
        assert_eq!(config.limits.output_images, 3);
        assert_eq!(config.limits.idle_timeout_ms, 30 * 60 * 1000);
    }

    #[test]
    fn a_caller_can_only_ask_for_less_than_the_ceiling() {
        let limits = ComposeConfig::builtin().limits;
        assert_eq!(limits.deadline_for(None), 30_000);
        assert_eq!(limits.deadline_for(Some(500)), 500);
        assert_eq!(limits.deadline_for(Some(10_000_000)), 120_000);
        assert_eq!(limits.deadline_for(Some(0)), 1);
        assert_eq!(limits.budget_for(None), 64);
        assert_eq!(limits.budget_for(Some(100_000)), 512);
        assert_eq!(limits.budget_for(Some(0)), 1);
    }

    #[test]
    fn a_mandatory_terminal_code_cannot_be_removed() {
        let text = SHIPPED.replace("  - COMPUTER_STOPPED_BY_USER\n", "");
        assert!(ComposeConfig::from_yaml(&text).is_err());
        let text = SHIPPED.replace("deadline_max_ms: 120000", "deadline_max_ms: 9999999");
        assert!(ComposeConfig::from_yaml(&text).is_err());
    }
}
