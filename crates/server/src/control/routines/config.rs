//! `config/automation.yaml` (ADP-007, DOMAIN.md §13.8): limits and bounds for inbound hooks, per-plan routine limits, dry runs
//! and workflow scripts, validated against `schemas/json/automation-config.schema.json` by the repository gates. Policy lives in
//! the file, never as a source constant (D-018).

use std::collections::BTreeMap;

use serde::Deserialize;

/// The shipped policy.
const SHIPPED: &str = include_str!("../../../../../config/automation.yaml");

/// The whole file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationConfig {
    /// Always 1.
    pub version: u32,
    /// Inbound hooks.
    pub hooks: HooksConfig,
    /// Limits by plan.
    pub limits: PlanLimitsTable,
    /// Limits for one tenant, by tenant id.
    pub tenant_overrides: BTreeMap<String, PlanOverride>,
    /// `DryRunRoutine`.
    pub dry_run: DryRunConfig,
    /// Workflow scripts.
    pub script: ScriptConfig,
}

/// `hooks`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    /// The largest body accepted.
    pub max_body_bytes: usize,
    /// How far a signature timestamp may be from the server's clock.
    pub tolerance_seconds: i64,
    /// Requests per endpoint per minute.
    pub rate_per_minute: u32,
    /// How long a delivery's identity is remembered.
    pub delivery_retention_days: i64,
}

/// `limits`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanLimitsTable {
    /// A personal tenant.
    pub personal: PlanLimits,
    /// Any other tenant.
    pub organization: PlanLimits,
}

/// What one plan allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanLimits {
    /// Active routines a workspace may hold.
    pub max_enabled_triggers: u64,
    /// The shortest interval or gap between windows.
    pub min_interval_seconds: u64,
}

/// A tenant's replacement for some of its plan's values.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanOverride {
    /// Replacement for `max_enabled_triggers`.
    pub max_enabled_triggers: Option<u64>,
    /// Replacement for `min_interval_seconds`.
    pub min_interval_seconds: Option<u64>,
}

/// `dry_run`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DryRunConfig {
    /// Dry runs a tenant may make a minute.
    pub rate_per_minute: u32,
    /// Upcoming windows shown for a schedule.
    pub next_windows: usize,
}

/// `script`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptConfig {
    /// Steps a script may have.
    pub max_steps: usize,
    /// Items a `parallel` step may fan out to.
    pub max_items: usize,
    /// The most calls a script may declare, and an approval may raise it to.
    pub max_agent_calls_ceiling: u64,
    /// Calls above which a gate is required.
    pub approval_threshold_calls: u64,
    /// The largest worker reply that counts as a result.
    pub max_result_bytes: usize,
    /// Workers one script run executes at once.
    pub max_parallel_calls: usize,
    /// How long an approval for a gate stays open.
    pub gate_ttl_seconds: i64,
}

impl AutomationConfig {
    /// The shipped `config/automation.yaml`.
    ///
    /// # Panics
    /// If the file the binary was built with does not parse: the repository gates parse it too.
    #[must_use]
    pub fn shipped() -> Self {
        serde_yaml::from_str(SHIPPED).expect("config/automation.yaml parses")
    }

    /// The limits that apply to a tenant: its plan's, with its own overrides on top.
    #[must_use]
    pub fn limits_for(&self, tenant_id: &str, personal: bool) -> PlanLimits {
        let plan = if personal {
            self.limits.personal
        } else {
            self.limits.organization
        };
        match self.tenant_overrides.get(tenant_id) {
            Some(over) => PlanLimits {
                max_enabled_triggers: over
                    .max_enabled_triggers
                    .unwrap_or(plan.max_enabled_triggers),
                min_interval_seconds: over
                    .min_interval_seconds
                    .unwrap_or(plan.min_interval_seconds),
            },
            None => plan,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_policy_loads_and_a_tenant_override_replaces_only_what_it_names() {
        let mut config = AutomationConfig::shipped();
        assert_eq!(
            config.hooks.max_body_bytes, 65_536,
            "the body cap is 64 KiB"
        );
        assert_eq!(config.script.approval_threshold_calls, 20);
        assert!(config.hooks.delivery_retention_days * 86_400 >= config.hooks.tolerance_seconds);
        let tenant = "tn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE";
        assert_eq!(
            config.limits_for(tenant, true),
            config.limits.personal,
            "a personal tenant gets the personal plan"
        );
        assert_eq!(config.limits_for(tenant, false), config.limits.organization);
        config.tenant_overrides.insert(
            tenant.to_string(),
            PlanOverride {
                max_enabled_triggers: Some(3),
                min_interval_seconds: None,
            },
        );
        let limits = config.limits_for(tenant, false);
        assert_eq!(limits.max_enabled_triggers, 3);
        assert_eq!(
            limits.min_interval_seconds,
            config.limits.organization.min_interval_seconds
        );
    }
}
