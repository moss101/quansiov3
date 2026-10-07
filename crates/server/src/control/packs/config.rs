//! `config/pack-install.yaml` (ADP-008, DOMAIN.md 11.6): bounds for installing a pack and for its install reports,
//! validated against `schemas/json/pack-install-config.schema.json` by the repository gates. Policy lives in the
//! file, never as a source constant (D-018).

use serde::Deserialize;

/// The shipped policy.
const SHIPPED: &str = include_str!("../../../../../config/pack-install.yaml");

/// The whole file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackConfig {
    /// Always 1.
    pub version: u32,
    /// Installing.
    pub install: InstallConfig,
    /// Install reports.
    pub reports: ReportsConfig,
}

/// `install`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallConfig {
    /// A pack's manifest and files, in bytes.
    pub max_content_bytes: usize,
    /// Files a pack may hold.
    pub max_files: usize,
    /// One file, in bytes.
    pub max_file_bytes: usize,
    /// Revisions kept for rollback besides the active one.
    pub retain_previous_revisions: usize,
    /// How long a report may stay `running` before it is closed as interrupted.
    pub running_grace_seconds: i64,
    /// Where built-in packs are read from.
    pub builtin_root: String,
}

/// `reports`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportsConfig {
    /// How long a report is kept.
    pub retention_days: i64,
    /// Diagnostics one report keeps.
    pub max_diagnostics: usize,
    /// One diagnostic's message, in bytes.
    pub max_diagnostic_bytes: usize,
    /// Reports one read returns.
    pub max_listed: i64,
}

impl PackConfig {
    /// The shipped `config/pack-install.yaml`.
    ///
    /// # Panics
    /// If the file the binary was built with does not parse: the repository gates parse it too.
    #[must_use]
    pub fn shipped() -> Self {
        serde_yaml::from_str(SHIPPED).expect("config/pack-install.yaml parses")
    }

    /// The shipped policy, parsed once.
    #[must_use]
    pub fn committed() -> &'static Self {
        static COMMITTED: std::sync::OnceLock<PackConfig> = std::sync::OnceLock::new();
        COMMITTED.get_or_init(Self::shipped)
    }

    /// The shipped file's source, for the schema gate.
    #[must_use]
    pub const fn shipped_source() -> &'static str {
        SHIPPED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_policy_retains_a_previous_revision_and_expires_reports() {
        let config = PackConfig::shipped();
        assert!(config.install.retain_previous_revisions >= 1);
        assert_eq!(config.reports.retention_days, 30);
        assert!(config.install.max_file_bytes <= config.install.max_content_bytes);
        assert!(config.reports.max_diagnostics >= 1 && config.reports.max_diagnostic_bytes >= 64);
    }
}
