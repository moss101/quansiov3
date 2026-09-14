//! Server-side feature flags from `config/flags.yaml` (APP-001, DOSSIER.md §18).
//!
//! Flags gate rollout. They never widen capability, bypass policy, skip approval or
//! write the Effect Ledger.

/// Embedded flag catalog. Authority stays in `config/`; this is the evaluation copy.
const FLAGS_YAML: &str = include_str!("../../../../config/flags.yaml");

/// Server-evaluated feature flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureFlags {
    values: Vec<(String, String)>,
}

impl FeatureFlags {
    /// Load the committed catalog.
    ///
    /// # Errors
    /// Returns a message when the YAML is not a flag map.
    pub fn load() -> Result<Self, String> {
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(FLAGS_YAML).map_err(|error| error.to_string())?;
        let flags = parsed
            .get("flags")
            .and_then(serde_yaml::Value::as_mapping)
            .ok_or_else(|| "config/flags.yaml has no flags map".to_string())?;
        let mut values = Vec::new();
        for (key, value) in flags {
            let key = key.as_str().unwrap_or_default().to_string();
            let rendered = match value {
                serde_yaml::Value::Bool(flag) => flag.to_string(),
                serde_yaml::Value::Number(number) => number.to_string(),
                serde_yaml::Value::String(text) => text.clone(),
                other => format!("{other:?}"),
            };
            values.push((key, rendered));
        }
        values.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(Self { values })
    }

    /// The raw flag value, if declared.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Boolean evaluation; missing flags are off.
    #[must_use]
    pub fn enabled(&self, name: &str) -> bool {
        matches!(self.get(name), Some("true" | "1" | "yes"))
    }

    /// Every flag, sorted, for diagnostics. Not an authority surface.
    #[must_use]
    pub fn all(&self) -> &[(String, String)] {
        &self.values
    }
}
