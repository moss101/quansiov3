//! Rebuild UsageRecords from RuntimeEvents. Billing is this projection, not runtime truth.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use quansio_core::{Prefix, Ulid};
use quansio_events::RuntimeEvent;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::meters::UsageMeter;

/// One projected UsageRecord (DOMAIN.md §13.4).
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRecord {
    /// `use_` identity, a pure function of (tenant, source_event_id, meter).
    pub id: String,
    /// Tenant.
    pub tenant_id: String,
    /// Workspace, when the source event named one.
    pub workspace_id: Option<String>,
    /// Scope refs copied from the source payload.
    pub scope_refs: Value,
    /// Meter.
    pub meter: UsageMeter,
    /// Quantity.
    pub quantity: f64,
    /// Unit.
    pub unit: &'static str,
    /// Optional cost.
    pub cost_minor_units: Option<i64>,
    /// Source RuntimeEvent id.
    pub source_event_id: String,
    /// When the source event occurred.
    pub occurred_at: DateTime<Utc>,
}

impl UsageRecord {
    /// Canonical identity: same inputs always mint the same `use_` id.
    #[must_use]
    pub fn identity_for(tenant_id: &str, source_event_id: &str, meter: UsageMeter) -> String {
        let mut hasher = Sha256::new();
        hasher.update(tenant_id.as_bytes());
        hasher.update([0u8]);
        hasher.update(source_event_id.as_bytes());
        hasher.update([0u8]);
        hasher.update(meter.as_str().as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        format!(
            "{}{}",
            Prefix::UsageRecord.as_str(),
            Ulid::from_bytes(bytes).to_base32()
        )
    }
}

/// Totals by meter, used for quota checks. Not a second authority — derived from records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageProjection {
    records: Vec<UsageRecord>,
}

impl UsageProjection {
    /// Empty projection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from source events. Order of `events` does not change the record set:
    /// records are keyed by (tenant, source_event_id, meter) and sorted canonically.
    #[must_use]
    pub fn rebuild(events: &[RuntimeEvent]) -> Self {
        let mut by_key: BTreeMap<(String, String, UsageMeter), UsageRecord> = BTreeMap::new();
        for event in events {
            for record in project_event(event) {
                by_key.insert(
                    (
                        record.tenant_id.clone(),
                        record.source_event_id.clone(),
                        record.meter,
                    ),
                    record,
                );
            }
        }
        Self {
            records: by_key.into_values().collect(),
        }
    }

    /// Projected records in canonical order.
    #[must_use]
    pub fn records(&self) -> &[UsageRecord] {
        &self.records
    }

    /// Sum of one meter.
    #[must_use]
    pub fn total(&self, meter: UsageMeter) -> f64 {
        self.records
            .iter()
            .filter(|record| record.meter == meter)
            .map(|record| record.quantity)
            .sum()
    }

    /// Append records from a newly admitted increment (quota path only).
    pub(crate) fn push(&mut self, record: UsageRecord) {
        let key = (
            record.tenant_id.clone(),
            record.source_event_id.clone(),
            record.meter,
        );
        if self.records.iter().any(|existing| {
            existing.tenant_id == key.0
                && existing.source_event_id == key.1
                && existing.meter == key.2
        }) {
            return;
        }
        self.records.push(record);
        self.records.sort_by(|left, right| {
            (
                left.tenant_id.as_str(),
                left.source_event_id.as_str(),
                left.meter,
            )
                .cmp(&(
                    right.tenant_id.as_str(),
                    right.source_event_id.as_str(),
                    right.meter,
                ))
        });
    }
}

/// Extract UsageRecords from one RuntimeEvent. Unknown types contribute nothing.
#[must_use]
pub fn project_event(event: &RuntimeEvent) -> Vec<UsageRecord> {
    let ty = event.event_type.to_string();
    let payload = &event.payload;
    match ty.as_str() {
        "model.usage_recorded" => meters_from_model(event, payload),
        "effect.settled" if payload.get("connector_id").is_some() => vec![record(
            event,
            UsageMeter::ConnectorCalls,
            1.0,
            payload.get("cost_minor_units").and_then(Value::as_i64),
        )],
        "target.stopped" => optional_quantity(
            event,
            payload,
            "machine_seconds",
            UsageMeter::MachineSeconds,
        ),
        "browser.session_closed" => optional_quantity(
            event,
            payload,
            "browser_seconds",
            UsageMeter::BrowserSeconds,
        ),
        "artifact.version_added" => {
            optional_quantity(event, payload, "size_bytes", UsageMeter::StorageBytes)
        }
        _ => Vec::new(),
    }
}

fn meters_from_model(event: &RuntimeEvent, payload: &Value) -> Vec<UsageRecord> {
    let mut out = Vec::new();
    let pairs = [
        ("input_tokens", UsageMeter::ModelInputTokens),
        ("output_tokens", UsageMeter::ModelOutputTokens),
        ("cache_tokens", UsageMeter::ModelCacheTokens),
        ("cost_minor_units", UsageMeter::ModelCost),
    ];
    for (field, meter) in pairs {
        if let Some(quantity) = number_field(payload, field) {
            let cost = if meter == UsageMeter::ModelCost {
                Some(quantity as i64)
            } else {
                payload.get("cost_minor_units").and_then(Value::as_i64)
            };
            out.push(record(event, meter, quantity, cost));
        }
    }
    out
}

fn optional_quantity(
    event: &RuntimeEvent,
    payload: &Value,
    field: &str,
    meter: UsageMeter,
) -> Vec<UsageRecord> {
    match number_field(payload, field) {
        Some(quantity) if quantity > 0.0 => vec![record(event, meter, quantity, None)],
        _ => Vec::new(),
    }
}

fn number_field(payload: &Value, field: &str) -> Option<f64> {
    payload.get(field).and_then(Value::as_f64).or_else(|| {
        payload
            .get(field)
            .and_then(Value::as_i64)
            .map(|value| value as f64)
    })
}

fn record(
    event: &RuntimeEvent,
    meter: UsageMeter,
    quantity: f64,
    cost_minor_units: Option<i64>,
) -> UsageRecord {
    let source_event_id = event.event_id.to_string();
    UsageRecord {
        id: UsageRecord::identity_for(&event.tenant_id, &source_event_id, meter),
        tenant_id: event.tenant_id.clone(),
        workspace_id: event.workspace_id.clone(),
        scope_refs: event.payload.get("scope_refs").cloned().unwrap_or_else(|| {
            serde_json::json!({
                "run_id": event.payload.get("run_id"),
                "agent_thread_id": event.payload.get("agent_thread_id"),
                "target_id": event.payload.get("target_id"),
                "connector_id": event.payload.get("connector_id"),
            })
        }),
        meter,
        quantity,
        unit: meter.unit(),
        cost_minor_units,
        source_event_id,
        occurred_at: event.occurred_at,
    }
}

/// A proposed increment, used by the quota gate before an effect is reserved.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDelta {
    /// Tenant.
    pub tenant_id: String,
    /// Workspace.
    pub workspace_id: Option<String>,
    /// Meter.
    pub meter: UsageMeter,
    /// Quantity to add.
    pub quantity: f64,
    /// Idempotency: the event that would produce this record.
    pub source_event_id: String,
    /// When.
    pub occurred_at: DateTime<Utc>,
}

impl UsageDelta {
    /// Materialise the record this delta would project.
    #[must_use]
    pub fn into_record(self) -> UsageRecord {
        UsageRecord {
            id: UsageRecord::identity_for(&self.tenant_id, &self.source_event_id, self.meter),
            tenant_id: self.tenant_id,
            workspace_id: self.workspace_id,
            scope_refs: serde_json::json!({}),
            meter: self.meter,
            quantity: self.quantity,
            unit: self.meter.unit(),
            cost_minor_units: None,
            source_event_id: self.source_event_id,
            occurred_at: self.occurred_at,
        }
    }
}

impl From<&UsageRecord> for UsageDelta {
    fn from(record: &UsageRecord) -> Self {
        Self {
            tenant_id: record.tenant_id.clone(),
            workspace_id: record.workspace_id.clone(),
            meter: record.meter,
            quantity: record.quantity,
            source_event_id: record.source_event_id.clone(),
            occurred_at: record.occurred_at,
        }
    }
}
