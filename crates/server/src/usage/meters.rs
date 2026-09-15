//! UsageRecord meters (DOMAIN.md §13.4). Distinct from runtime budget meters (RUN-010).

use super::UsageError;

/// One UsageRecord meter. Billing projects these; the runtime does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageMeter {
    /// Model prompt tokens.
    ModelInputTokens,
    /// Model completion tokens.
    ModelOutputTokens,
    /// Prompt-cache tokens.
    ModelCacheTokens,
    /// Model cost in minor currency units.
    ModelCost,
    /// Execution-target machine time.
    MachineSeconds,
    /// Artifact/object storage.
    StorageBytes,
    /// Connector calls.
    ConnectorCalls,
    /// Browser session seconds.
    BrowserSeconds,
}

impl UsageMeter {
    /// Every meter, in DOMAIN.md §13.4 order.
    pub const ALL: [Self; 8] = [
        Self::ModelInputTokens,
        Self::ModelOutputTokens,
        Self::ModelCacheTokens,
        Self::ModelCost,
        Self::MachineSeconds,
        Self::StorageBytes,
        Self::ConnectorCalls,
        Self::BrowserSeconds,
    ];

    /// Canonical database/wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelInputTokens => "model_input_tokens",
            Self::ModelOutputTokens => "model_output_tokens",
            Self::ModelCacheTokens => "model_cache_tokens",
            Self::ModelCost => "model_cost",
            Self::MachineSeconds => "machine_seconds",
            Self::StorageBytes => "storage_bytes",
            Self::ConnectorCalls => "connector_calls",
            Self::BrowserSeconds => "browser_seconds",
        }
    }

    /// Unit for this meter.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::ModelInputTokens | Self::ModelOutputTokens | Self::ModelCacheTokens => "tokens",
            Self::ModelCost => "minor_units",
            Self::MachineSeconds | Self::BrowserSeconds => "seconds",
            Self::StorageBytes => "bytes",
            Self::ConnectorCalls => "calls",
        }
    }

    /// Parse a wire spelling.
    ///
    /// # Errors
    /// Unknown meter names are refused.
    pub fn parse(value: &str) -> Result<Self, UsageError> {
        Self::ALL
            .into_iter()
            .find(|meter| meter.as_str() == value)
            .ok_or_else(|| UsageError::UnknownMeter {
                meter: value.to_string(),
            })
    }
}

impl std::fmt::Display for UsageMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
