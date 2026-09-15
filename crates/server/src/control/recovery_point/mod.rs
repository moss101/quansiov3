//! RecoveryConsistencyPoint: DB/event/artifact/target/effect watermark (OPS-005).
//!
//! A restore is consistent when it lands on one of these points: the event sequence,
//! artifact manifest digest, target snapshots and Effect Ledger settlement watermark
//! describe the same instant. Settled effects at that instant must still be settled
//! after restore — never pending, never duplicated. RPO/RTO are durations measured
//! on the drill, not configured hopes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use sha2::{Digest, Sha256};

/// Effect statuses that are settlement-terminal (DOMAIN.md §7.2).
pub const SETTLED: &[&str] = &[
    "SETTLED_SUCCESS",
    "SETTLED_FAILED",
    "RECONCILED_SUCCESS",
    "RECONCILED_FAILED",
    "RECONCILIATION_MANUAL",
    "DENIED",
    "EXPIRED",
    "CANCELLED",
];

/// One captured restore point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryConsistencyPoint {
    /// Tenant the point belongs to.
    pub tenant_id: String,
    /// Highest RuntimeEvent sequence included.
    pub event_sequence: i64,
    /// sha256 of sorted `artifact_version_id:content_digest` lines.
    pub artifact_manifest_digest: String,
    /// Target snapshot ids included.
    pub target_snapshot_ids: Vec<String>,
    /// Settled effect identities at this instant.
    pub settled_effect_ids: Vec<String>,
    /// Postgres WAL LSN when captured, if known.
    pub db_lsn: Option<String>,
    /// Measured RPO: time between last durable event and capture.
    pub rpo_ms: u64,
    /// Measured RTO: restore duration. Zero until a drill finishes.
    pub rto_ms: u64,
}

/// Live durable state a restore drill mutates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableSlice {
    /// Events (id, sequence).
    pub events: Vec<(String, i64)>,
    /// Effects: id → status.
    pub effects: BTreeMap<String, String>,
    /// Artifact versions: id → content digest.
    pub artifacts: BTreeMap<String, String>,
    /// Target snapshot ids.
    pub target_snapshots: Vec<String>,
    /// Timestamp of the last durable event (ms).
    pub last_event_at_ms: u64,
}

impl DurableSlice {
    /// Capture a consistency point. `now_ms` is the drill clock.
    #[must_use]
    pub fn capture(&self, tenant_id: impl Into<String>, now_ms: u64) -> RecoveryConsistencyPoint {
        let event_sequence = self.events.iter().map(|(_, seq)| *seq).max().unwrap_or(0);
        let settled_effect_ids = self
            .effects
            .iter()
            .filter(|(_, status)| SETTLED.contains(&status.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        RecoveryConsistencyPoint {
            tenant_id: tenant_id.into(),
            event_sequence,
            artifact_manifest_digest: manifest_digest(&self.artifacts),
            target_snapshot_ids: self.target_snapshots.clone(),
            settled_effect_ids,
            db_lsn: None,
            rpo_ms: now_ms.saturating_sub(self.last_event_at_ms),
            rto_ms: 0,
        }
    }

    /// Restore this slice to `point`. Returns the restored slice and measured RTO.
    #[must_use]
    pub fn restore(
        &self,
        point: &RecoveryConsistencyPoint,
        restore_started: Instant,
    ) -> (Self, u64) {
        let mut effects = BTreeMap::new();
        for id in &point.settled_effect_ids {
            if let Some(status) = self.effects.get(id) {
                effects.insert(id.clone(), status.clone());
            }
        }
        let restored = Self {
            events: self
                .events
                .iter()
                .filter(|(_, seq)| *seq <= point.event_sequence)
                .cloned()
                .collect(),
            effects,
            artifacts: filter_artifacts(&self.artifacts, &point.artifact_manifest_digest),
            target_snapshots: point.target_snapshot_ids.clone(),
            last_event_at_ms: self.last_event_at_ms,
        };
        let rto_ms = u64::try_from(restore_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        (restored, rto_ms)
    }
}

fn filter_artifacts(
    all: &BTreeMap<String, String>,
    captured_digest: &str,
) -> BTreeMap<String, String> {
    if manifest_digest(all) == captured_digest {
        return all.clone();
    }
    // Drop keys from the end until the digest matches the captured one.
    let mut keys: Vec<_> = all.keys().cloned().collect();
    keys.sort();
    while !keys.is_empty() {
        let subset: BTreeMap<_, _> = keys
            .iter()
            .map(|key| (key.clone(), all[key].clone()))
            .collect();
        if manifest_digest(&subset) == captured_digest {
            return subset;
        }
        keys.pop();
    }
    BTreeMap::new()
}

fn manifest_digest(artifacts: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (id, digest) in artifacts {
        hasher.update(id.as_bytes());
        hasher.update(b":");
        hasher.update(digest.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// Audit that restore preserved settlement and uniqueness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementAudit {
    /// Settled ids that are no longer settled (must be empty).
    pub became_pending: Vec<String>,
    /// Duplicate effect identities (must be empty).
    pub duplicates: Vec<String>,
    /// Whether the restored tenant is usable.
    pub tenant_usable: bool,
}

impl SettlementAudit {
    /// Compare pre-restore settled set to restored effects.
    #[must_use]
    pub fn of(point: &RecoveryConsistencyPoint, restored: &DurableSlice) -> Self {
        let mut seen = BTreeSet::new();
        let mut duplicates = Vec::new();
        for id in restored.effects.keys() {
            if !seen.insert(id.clone()) {
                duplicates.push(id.clone());
            }
        }
        let became_pending = point
            .settled_effect_ids
            .iter()
            .filter(|id| match restored.effects.get(*id) {
                Some(status) => !SETTLED.contains(&status.as_str()),
                None => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        let tenant_usable = became_pending.is_empty() && duplicates.is_empty();
        Self {
            became_pending,
            duplicates,
            tenant_usable,
        }
    }
}

/// Validate a restored slice against its point.
///
/// # Errors
/// Returns a message when sequence, artifacts, snapshots or settlement drifted.
pub fn validate_point(
    point: &RecoveryConsistencyPoint,
    restored: &DurableSlice,
) -> Result<(), String> {
    let max_seq = restored
        .events
        .iter()
        .map(|(_, seq)| *seq)
        .max()
        .unwrap_or(0);
    if max_seq > point.event_sequence {
        return Err(format!(
            "restored event sequence {max_seq} is after the point {}",
            point.event_sequence
        ));
    }
    if manifest_digest(&restored.artifacts) != point.artifact_manifest_digest {
        return Err("artifact manifest digest drifted across restore".to_string());
    }
    if restored.target_snapshots != point.target_snapshot_ids {
        return Err("target snapshots drifted across restore".to_string());
    }
    let audit = SettlementAudit::of(point, restored);
    if !audit.tenant_usable {
        return Err(format!(
            "settlement audit failed: pending={:?} dup={:?}",
            audit.became_pending, audit.duplicates
        ));
    }
    Ok(())
}

/// Repository path.
pub const RECOVERY_POINT_OWNER: &str = "crates/server/src/control/recovery_point";
