//! Operator kill switches (OPS-008, DOSSIER.md §21.5).
//!
//! Each switch is a `runtime.control` effect: audited, reversible, and scoped.
//! Effect freeze blocks *new* tier ≥ 2 consequential actions. Reads and evidence
//! capture continue. A freeze never rewrites an in-flight EffectRecord.

use std::collections::BTreeMap;

/// The kill switches DOSSIER.md §21.5 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KillSwitch {
    /// Disable a model provider.
    ProviderDisable,
    /// Revoke a connector.
    ConnectorRevoke,
    /// Quarantine a worker.
    WorkerQuarantine,
    /// Drain an execution target.
    TargetDrain,
    /// Freeze new tier ≥ 2 effects.
    EffectFreeze,
    /// Emergency policy deny.
    PolicyEmergencyDeny,
    /// Shed slow stream consumers.
    StreamShed,
}

impl KillSwitch {
    /// Every switch, in dossier order.
    pub const ALL: [Self; 7] = [
        Self::ProviderDisable,
        Self::ConnectorRevoke,
        Self::WorkerQuarantine,
        Self::TargetDrain,
        Self::EffectFreeze,
        Self::PolicyEmergencyDeny,
        Self::StreamShed,
    ];

    /// Canonical name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderDisable => "provider.disable",
            Self::ConnectorRevoke => "connector.revoke",
            Self::WorkerQuarantine => "worker.quarantine",
            Self::TargetDrain => "target.drain",
            Self::EffectFreeze => "effect.freeze",
            Self::PolicyEmergencyDeny => "policy.emergency_deny",
            Self::StreamShed => "stream.shed",
        }
    }
}

/// One audited engage/release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchAudit {
    /// Switch.
    pub switch: KillSwitch,
    /// Target (provider id, worker id, `*` for tenant-wide).
    pub target: String,
    /// `engaged` or `released`.
    pub action: &'static str,
    /// Operator.
    pub actor: String,
}

/// Operator board. Default: every switch released.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillSwitchBoard {
    engaged: BTreeMap<(KillSwitch, String), String>,
    audit: Vec<SwitchAudit>,
    /// Evidence ids captured while frozen — must remain readable.
    evidence: Vec<String>,
}

impl KillSwitchBoard {
    /// Engage a switch. Audited.
    pub fn engage(
        &mut self,
        switch: KillSwitch,
        target: impl Into<String>,
        actor: impl Into<String>,
    ) {
        let target = target.into();
        let actor = actor.into();
        self.engaged.insert((switch, target.clone()), actor.clone());
        self.audit.push(SwitchAudit {
            switch,
            target,
            action: "engaged",
            actor,
        });
    }

    /// Release a switch. Audited and recoverable.
    pub fn release(&mut self, switch: KillSwitch, target: &str, actor: impl Into<String>) -> bool {
        let actor = actor.into();
        let removed = self.engaged.remove(&(switch, target.to_string())).is_some();
        if removed {
            self.audit.push(SwitchAudit {
                switch,
                target: target.to_string(),
                action: "released",
                actor,
            });
        }
        removed
    }

    /// Whether a switch is engaged for `target` or tenant-wide `*`.
    #[must_use]
    pub fn is_engaged(&self, switch: KillSwitch, target: &str) -> bool {
        self.engaged.contains_key(&(switch, target.to_string()))
            || self.engaged.contains_key(&(switch, "*".to_string()))
    }

    /// Effect freeze: new tier ≥ 2 actions are blocked. Reads/evidence are not.
    #[must_use]
    pub fn allows_new_effect(&self, tier: u8) -> bool {
        if self.is_engaged(KillSwitch::EffectFreeze, "*") {
            return tier < 2;
        }
        true
    }

    /// Capture evidence while frozen — must succeed.
    pub fn capture_evidence(&mut self, evidence_id: impl Into<String>) {
        self.evidence.push(evidence_id.into());
    }

    /// Evidence captured under freeze.
    #[must_use]
    pub fn evidence(&self) -> &[String] {
        &self.evidence
    }

    /// Audit trail, oldest first.
    #[must_use]
    pub fn audit(&self) -> &[SwitchAudit] {
        &self.audit
    }

    /// Worker quarantine.
    #[must_use]
    pub fn worker_quarantined(&self, worker_id: &str) -> bool {
        self.is_engaged(KillSwitch::WorkerQuarantine, worker_id)
    }

    /// Provider disabled.
    #[must_use]
    pub fn provider_disabled(&self, provider: &str) -> bool {
        self.is_engaged(KillSwitch::ProviderDisable, provider)
    }
}

/// Repository path.
pub const OPS_OWNER: &str = "crates/server/src/control/ops";
