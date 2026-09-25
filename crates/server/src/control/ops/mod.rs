//! Operator kill switches (OPS-008, DOSSIER.md §21.5, DOMAIN.md §7.6).
//!
//! The canonical owner of switch state (`ops_switches`). One row per engagement, kept as
//! history, with at most one active row per `(tenant, switch, target)`.
//!
//! Engaging and releasing are `runtime.control` actions (tier 3, tenant administrators). Each
//! commits with its `ops.kill_switch_*` RuntimeEvent and its AuditEntry, and releasing restores
//! service without deleting anything.
//!
//! Enforcement belongs to the owners of what a switch stops, and they read [`OpsGate`]:
//! - runtime dispatch: freeze, emergency deny, quarantine;
//! - the model source: disabled providers;
//! - the live stream hub: shed.
//!
//! Where a switch changes another owner's state, this module calls that owner:
//! - the target lifecycle, for drain and undrain;
//! - lease revocation, for quarantine;
//! - the connector instance, for revoke.
//!
//! [`runbook_markdown`] renders `docs/runbooks/kill-switches.md` from the catalog below, so the
//! runbook cannot drift from the switches it describes.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_events::{EventError, EventStore};
use quansio_machine::control::{MachineControl, TargetFence, TargetStatus};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::control::connectors::{ConnectorBroker, ConnectorState};
use crate::control::identity::enterprise::{audit_in, stage_in};
use crate::control::identity::Acting;
use crate::control::schema;

/// Repository path of this owner, named in refusals.
pub const OPS_OWNER: &str = "crates/server/src/control/ops";

/// The kill switches DOSSIER.md §21.5 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KillSwitch {
    /// Stop routing model calls to a provider.
    ProviderDisable,
    /// Revoke a connector instance.
    ConnectorRevoke,
    /// Fence the worker of an execution target.
    WorkerQuarantine,
    /// Drain an execution target.
    TargetDrain,
    /// Freeze new tier ≥ 2 effects.
    EffectFreeze,
    /// Deny matching effect classes outright.
    PolicyEmergencyDeny,
    /// Disconnect live stream subscribers.
    StreamShed,
}

/// One catalog entry: what the switch targets, what it does, how it is recovered.
#[derive(Debug, Clone, Copy)]
pub struct SwitchSpec {
    /// The switch.
    pub switch: KillSwitch,
    /// The command that engages it (besides `EngageKillSwitch`).
    pub engage_command: &'static str,
    /// The command that releases it (besides `ReleaseKillSwitch`).
    pub release_command: &'static str,
    /// What a target is.
    pub target: &'static str,
    /// What happens while engaged.
    pub effect: &'static str,
    /// What is untouched.
    pub keeps: &'static str,
    /// How service comes back.
    pub recovery: &'static str,
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

    /// Parse a canonical name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|switch| switch.as_str() == value)
    }

    /// The runbook entry.
    #[must_use]
    pub const fn spec(self) -> SwitchSpec {
        match self {
            Self::ProviderDisable => SwitchSpec {
                switch: self,
                engage_command: "DisableProvider {provider, reason}",
                release_command: "EnableProvider {provider}",
                target: "a provider name from config/models.yaml (`anthropic`, `openai`, ...)",
                effect: "every model call names the provider as disabled; the gateway fails over to an enabled fallback route and refuses with PROVIDER_UNAVAILABLE when none is left",
                keeps: "calls already streaming finish; routes to other providers are unaffected",
                recovery: "release; the next model call may route to the provider again",
            },
            Self::ConnectorRevoke => SwitchSpec {
                switch: self,
                engage_command: "RevokeConnector {instance_id, reason}",
                release_command: "ReleaseKillSwitch {switch: connector.revoke, target}",
                target: "a connector instance id (`cnx_...`)",
                effect: "the instance is revoked and its credential handle cleared; no dispatch can use it",
                keeps: "evidence and artifacts produced through the connector",
                recovery: "reconnect with BeginConnectorAuth / CompleteConnectorAuth, then release to close the incident",
            },
            Self::WorkerQuarantine => SwitchSpec {
                switch: self,
                engage_command: "QuarantineWorker {target_id, reason}",
                release_command: "ReleaseKillSwitch {switch: worker.quarantine, target}",
                target: "an execution target id (`tgt_...`), the worker that serves it",
                effect: "the target's lease is revoked (its generation moves, fencing the worker); every tool call of a run on it is refused TARGET_UNAVAILABLE",
                keeps: "other targets and their workers; effects already in the ledger, which reconcile rather than re-dispatch",
                recovery: "release; runs on the target dispatch again and acquire a fresh lease",
            },
            Self::TargetDrain => SwitchSpec {
                switch: self,
                engage_command: "DrainTarget {target_id, reason}",
                release_command: "ReleaseKillSwitch {switch: target.drain, target}",
                target: "an execution target id (`tgt_...`) that is READY or BUSY",
                effect: "the target moves to DRAINING: no new lease is granted, in-flight work finishes",
                keeps: "the running lease and its work",
                recovery: "release returns a DRAINING target to READY; a target that already stopped is restored or replaced by its lifecycle",
            },
            Self::EffectFreeze => SwitchSpec {
                switch: self,
                engage_command: "FreezeEffects {workspace_id?, reason}",
                release_command: "UnfreezeEffects {workspace_id?}",
                target: "`*` for the tenant, or a workspace id (`ws_...`)",
                effect: "new tier >= 2 effects are refused EFFECTS_FROZEN before any effect is reserved or approval requested; an approved effect is refused before reservation",
                keeps: "tier 0/1 calls, every read, the evidence store, and effects already in the ledger",
                recovery: "release; runs propose again and nothing needs repair",
            },
            Self::PolicyEmergencyDeny => SwitchSpec {
                switch: self,
                engage_command: "EngageKillSwitch {switch: policy.emergency_deny, target, reason}",
                release_command: "ReleaseKillSwitch {switch: policy.emergency_deny, target}",
                target: "an effect class (`message.send`), a family prefix (`scm.*`), or `*`",
                effect: "matching tool calls are refused POLICY_DENIED before policy is evaluated",
                keeps: "calls of other classes; stored policies and user rules",
                recovery: "release; ordinary policy decides again",
            },
            Self::StreamShed => SwitchSpec {
                switch: self,
                engage_command: "EngageKillSwitch {switch: stream.shed, target: *, reason}",
                release_command: "ReleaseKillSwitch {switch: stream.shed, target: *}",
                target: "`*`",
                effect: "every live /v1/stream subscriber of the tenant is disconnected with STREAM_BACKPRESSURE; while engaged a lagging subscriber is disconnected instead of missing frames",
                keeps: "every durable event: clients reconnect with their cursor and lose nothing",
                recovery: "release; nothing else is needed",
            },
        }
    }
}

/// Refusals of an operator action.
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    /// A request that does not meet the contract.
    #[error("ops invalid: {0}")]
    Invalid(String),
    /// Nothing to release, or no such target.
    #[error("ops not found: {0}")]
    NotFound(String),
    /// Already engaged, or the target is in a state the switch cannot act on.
    #[error("ops conflict: {0}")]
    Conflict(String),
    /// The event store refused the commit.
    #[error("ops event store: {0}")]
    Event(EventError),
    /// Database failure.
    #[error("ops database: {0}")]
    Database(#[from] sqlx::Error),
}

impl OpsError {
    /// The DOMAIN.md §15 code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT_STATE",
            Self::Event(_) | Self::Database(_) => "INTERNAL",
        }
    }
}

fn rejected(error: &OpsError) -> EventError {
    EventError::MutationRejected {
        owner: OPS_OWNER,
        message: error.to_string(),
    }
}

fn unwrap_rejection(error: EventError) -> OpsError {
    match error {
        EventError::MutationRejected { message, .. } => {
            let strip = |prefix: &str| message.strip_prefix(prefix).map(str::to_string);
            if let Some(rest) = strip("ops not found: ") {
                OpsError::NotFound(rest)
            } else if let Some(rest) = strip("ops conflict: ") {
                OpsError::Conflict(rest)
            } else if let Some(rest) = strip("ops invalid: ") {
                OpsError::Invalid(rest)
            } else {
                OpsError::Invalid(message)
            }
        }
        other => OpsError::Event(other),
    }
}

/// Check a target's shape for its switch (existence is checked in the transaction).
fn check_target(switch: KillSwitch, target: &str) -> Result<(), OpsError> {
    let invalid = |why: &str| {
        Err(OpsError::Invalid(format!(
            "{target:?} is not a {} target: {why}",
            switch.as_str()
        )))
    };
    let typed = |prefix: Prefix| CanonicalId::parse_typed(target, prefix).is_ok();
    match switch {
        KillSwitch::EffectFreeze if target == "*" || typed(Prefix::Workspace) => Ok(()),
        KillSwitch::EffectFreeze => invalid("use * or a workspace id"),
        KillSwitch::StreamShed if target == "*" => Ok(()),
        KillSwitch::StreamShed => invalid("use *"),
        KillSwitch::WorkerQuarantine | KillSwitch::TargetDrain
            if typed(Prefix::ExecutionTarget) =>
        {
            Ok(())
        }
        KillSwitch::WorkerQuarantine | KillSwitch::TargetDrain => {
            invalid("use an execution target id")
        }
        KillSwitch::ConnectorRevoke if typed(Prefix::Connector) => Ok(()),
        KillSwitch::ConnectorRevoke => invalid("use a connector instance id"),
        KillSwitch::ProviderDisable
            if !target.is_empty()
                && target.len() <= 64
                && target.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                }) =>
        {
            Ok(())
        }
        KillSwitch::ProviderDisable => invalid("use a provider name"),
        KillSwitch::PolicyEmergencyDeny => {
            let class = target.strip_suffix(".*").unwrap_or(target);
            if target == "*"
                || (!class.is_empty()
                    && class.len() <= 128
                    && class.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_'
                    }))
            {
                Ok(())
            } else {
                invalid("use an effect class, <family>.* or *")
            }
        }
    }
}

/// The active switches that govern a tenant's work, read once per decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpsGate {
    frozen: Vec<String>,
    emergency: Vec<String>,
    quarantined: Vec<String>,
    disabled_providers: Vec<String>,
    shedding: bool,
}

impl OpsGate {
    /// Read the tenant's active switches.
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn load(pool: &PgPool, tenant_id: &str) -> Result<Self, sqlx::Error> {
        let mut tx = pool.begin().await?;
        schema::set_tenant_context(&mut tx, tenant_id)
            .await
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT switch, target FROM ops_switches WHERE tenant_id = $1 AND released_at IS NULL",
        )
        .bind(tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Self::from_rows(rows))
    }

    fn from_rows(rows: Vec<(String, String)>) -> Self {
        let mut gate = Self::default();
        for (switch, target) in rows {
            match KillSwitch::parse(&switch) {
                Some(KillSwitch::EffectFreeze) => gate.frozen.push(target),
                Some(KillSwitch::PolicyEmergencyDeny) => gate.emergency.push(target),
                Some(KillSwitch::WorkerQuarantine) => gate.quarantined.push(target),
                Some(KillSwitch::ProviderDisable) => gate.disabled_providers.push(target),
                Some(KillSwitch::StreamShed) => gate.shedding = true,
                _ => {}
            }
        }
        gate.disabled_providers.sort();
        gate
    }

    /// Whether a new effect of `tier` in `workspace_id` is frozen (tier ≥ 2 only).
    #[must_use]
    pub fn freezes(&self, workspace_id: &str, tier: u8) -> bool {
        tier >= 2
            && self
                .frozen
                .iter()
                .any(|target| target == "*" || target == workspace_id)
    }

    /// The emergency deny that covers `effect_class`, if one does.
    #[must_use]
    pub fn emergency_deny(&self, effect_class: &str) -> Option<&str> {
        self.emergency
            .iter()
            .find(|target| {
                *target == "*"
                    || *target == effect_class
                    || target.strip_suffix('*').is_some_and(|prefix| {
                        prefix.ends_with('.') && effect_class.starts_with(prefix)
                    })
            })
            .map(String::as_str)
    }

    /// Whether the worker of `target_id` is quarantined.
    #[must_use]
    pub fn quarantines(&self, target_id: &str) -> bool {
        self.quarantined.iter().any(|target| target == target_id)
    }

    /// Providers no model call may route to.
    #[must_use]
    pub fn disabled_providers(&self) -> &[String] {
        &self.disabled_providers
    }

    /// Whether live subscribers are being shed.
    #[must_use]
    pub const fn shedding(&self) -> bool {
        self.shedding
    }
}

/// A switch row as listed.
type SwitchRow = (
    String,
    String,
    String,
    String,
    Value,
    chrono::DateTime<chrono::Utc>,
    Option<Value>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
);

fn view(row: SwitchRow) -> Value {
    let (
        id,
        switch,
        target,
        reason,
        engaged_by,
        engaged_at,
        released_by,
        released_at,
        release_reason,
    ) = row;
    json!({
        "id": id,
        "switch": switch,
        "target": target,
        "reason": reason,
        "engaged_by": engaged_by,
        "engaged_at": engaged_at,
        "released_by": released_by,
        "released_at": released_at,
        "release_reason": release_reason,
        "active": released_at.is_none(),
    })
}

const COLUMNS: &str =
    "id, switch, target, reason, engaged_by, engaged_at, released_by, released_at, release_reason";

/// The kill-switch owner.
#[derive(Debug, Clone)]
pub struct OpsControl {
    pool: PgPool,
    events: EventStore,
}

impl OpsControl {
    /// Bind to a pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            events: EventStore::new(pool.clone()),
            pool,
        }
    }

    /// Engage `switch` on `target`: its owner-side effect, the row, the event and the audit entry.
    ///
    /// # Errors
    /// [`OpsError::Invalid`] for a malformed target or reason, [`OpsError::NotFound`] for a
    /// target that does not exist, [`OpsError::Conflict`] when already engaged or the target's
    /// state refuses the switch.
    pub async fn engage(
        &self,
        tenant_id: &str,
        acting: &Acting,
        switch: KillSwitch,
        target: &str,
        reason: &str,
    ) -> Result<Value, OpsError> {
        check_target(switch, target)?;
        let reason = reason.trim().to_string();
        if reason.is_empty() || reason.len() > 2000 {
            return Err(OpsError::Invalid(
                "a kill switch needs a reason (1 to 2000 characters)".to_string(),
            ));
        }
        // A connector revoke goes through the connector owner in its own transaction first:
        // revocation is terminal and idempotent, so a retry after a failure below is safe.
        if switch == KillSwitch::ConnectorRevoke {
            ConnectorBroker::new(self.pool.clone(), tenant_id)
                .transition(target, ConnectorState::Revoked)
                .await
                .map_err(|error| match error.to_string() {
                    text if text.contains("not found") => OpsError::NotFound(text),
                    text => OpsError::Conflict(text),
                })?;
        }
        let id = CanonicalId::generate(Prefix::KillSwitch, &mut UlidGenerator::new()).to_string();
        let tenant = tenant_id.to_string();
        let acting = acting.clone();
        let target = target.to_string();
        let row = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let engaged: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM ops_switches WHERE tenant_id = $1 \
                         AND switch = $2 AND target = $3 AND released_at IS NULL)",
                    )
                    .bind(&tenant)
                    .bind(switch.as_str())
                    .bind(&target)
                    .fetch_one(&mut **tx)
                    .await?;
                    if engaged {
                        return Err(rejected(&OpsError::Conflict(format!(
                            "{} is already engaged on {target}",
                            switch.as_str()
                        ))));
                    }
                    let side = engage_side_effect(tx, &tenant, switch, &target).await?;
                    let actor = serde_json::to_value(&acting.actor).unwrap_or(Value::Null);
                    let row: SwitchRow = sqlx::query_as(&format!(
                        "INSERT INTO ops_switches (id, tenant_id, switch, target, reason, engaged_by) \
                         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
                    ))
                    .bind(&id)
                    .bind(&tenant)
                    .bind(switch.as_str())
                    .bind(&target)
                    .bind(&reason)
                    .bind(&actor)
                    .fetch_one(&mut **tx)
                    .await?;
                    stage_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        ("kill_switch", &id),
                        "ops.kill_switch_engaged",
                        json!({ "switch": switch.as_str(), "target": target, "reason": reason, "side_effect": side }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        &format!("ops.{}.engage", switch.as_str()),
                        &format!("kill_switch:{}:{target}", switch.as_str()),
                        "engaged",
                        &format!("runtime.control (tier 3): {reason}"),
                    )
                    .await?;
                    Ok(row)
                })
            })
            .await
            .map_err(unwrap_rejection)?;
        Ok(view(row))
    }

    /// Release `switch` on `target`, undoing its owner-side effect where one can be undone.
    ///
    /// # Errors
    /// [`OpsError::NotFound`] when the switch is not engaged on that target.
    pub async fn release(
        &self,
        tenant_id: &str,
        acting: &Acting,
        switch: KillSwitch,
        target: &str,
        reason: Option<&str>,
    ) -> Result<Value, OpsError> {
        let tenant = tenant_id.to_string();
        let acting = acting.clone();
        let target = target.to_string();
        let reason = reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("released")
            .to_string();
        let row = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let id: Option<String> = sqlx::query_scalar(
                        "SELECT id FROM ops_switches WHERE tenant_id = $1 AND switch = $2 \
                         AND target = $3 AND released_at IS NULL FOR UPDATE",
                    )
                    .bind(&tenant)
                    .bind(switch.as_str())
                    .bind(&target)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some(id) = id else {
                        return Err(rejected(&OpsError::NotFound(format!(
                            "{} is not engaged on {target}",
                            switch.as_str()
                        ))));
                    };
                    let side = release_side_effect(tx, &tenant, switch, &target).await?;
                    let actor = serde_json::to_value(&acting.actor).unwrap_or(Value::Null);
                    let row: SwitchRow = sqlx::query_as(&format!(
                        "UPDATE ops_switches SET released_at = now(), released_by = $3, release_reason = $4 \
                         WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
                    ))
                    .bind(&tenant)
                    .bind(&id)
                    .bind(&actor)
                    .bind(&reason)
                    .fetch_one(&mut **tx)
                    .await?;
                    stage_in(
                        tx,
                        batch,
                        &tenant,
                        &acting,
                        ("kill_switch", &id),
                        "ops.kill_switch_released",
                        json!({ "switch": switch.as_str(), "target": target, "reason": reason, "side_effect": side }),
                    )
                    .await?;
                    audit_in(
                        tx,
                        &tenant,
                        &acting,
                        &format!("ops.{}.release", switch.as_str()),
                        &format!("kill_switch:{}:{target}", switch.as_str()),
                        "released",
                        &format!("runtime.control (tier 3): {reason}"),
                    )
                    .await?;
                    Ok(row)
                })
            })
            .await
            .map_err(unwrap_rejection)?;
        Ok(view(row))
    }

    /// `GET /v1/ops/switches`: active switches first, then the most recent history.
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn list(&self, tenant_id: &str, limit: i64) -> Result<Vec<Value>, OpsError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, tenant_id)
            .await
            .map_err(|error| OpsError::Invalid(error.to_string()))?;
        let rows: Vec<SwitchRow> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM ops_switches WHERE tenant_id = $1 \
             ORDER BY (released_at IS NULL) DESC, engaged_at DESC LIMIT $2"
        ))
        .bind(tenant_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(view).collect())
    }
}

/// The owner-side change a switch makes on engage, inside the switch's transaction.
async fn engage_side_effect(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    tenant: &str,
    switch: KillSwitch,
    target: &str,
) -> Result<Value, EventError> {
    match switch {
        KillSwitch::EffectFreeze if target != "*" => {
            let known: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM workspaces WHERE tenant_id = $1 AND id = $2)",
            )
            .bind(tenant)
            .bind(target)
            .fetch_one(&mut **tx)
            .await?;
            if !known {
                return Err(rejected(&OpsError::NotFound(format!(
                    "workspace {target} is not in this tenant"
                ))));
            }
            Ok(Value::Null)
        }
        KillSwitch::TargetDrain => {
            let current = load_target(tx, tenant, target).await?;
            if !matches!(current.status, TargetStatus::Ready | TargetStatus::Busy) {
                return Err(rejected(&OpsError::Conflict(format!(
                    "target {target} is {} and cannot be drained",
                    current.status.as_str()
                ))));
            }
            MachineControl::transition(
                tx,
                tenant,
                target,
                &TargetFence {
                    generation: current.generation,
                },
                TargetStatus::Draining,
            )
            .await
            .map_err(|error| rejected(&OpsError::Conflict(error.to_string())))?;
            Ok(json!({ "target_status": { "from": current.status.as_str(), "to": "DRAINING" } }))
        }
        KillSwitch::WorkerQuarantine => {
            let current = load_target(tx, tenant, target).await?;
            let Some(lease_id) = current.lease_id.clone() else {
                return Ok(json!({ "revoked_lease": null }));
            };
            MachineControl::revoke_lease(tx, tenant, &lease_id)
                .await
                .map_err(|error| rejected(&OpsError::Conflict(error.to_string())))?;
            Ok(json!({ "revoked_lease": lease_id }))
        }
        _ => Ok(Value::Null),
    }
}

/// The owner-side change a release undoes, inside the release's transaction.
async fn release_side_effect(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    tenant: &str,
    switch: KillSwitch,
    target: &str,
) -> Result<Value, EventError> {
    if switch != KillSwitch::TargetDrain {
        return Ok(Value::Null);
    }
    let current = load_target(tx, tenant, target).await?;
    if current.status != TargetStatus::Draining {
        // Already stopped (or failed): its lifecycle, not this release, brings it back.
        return Ok(json!({ "target_status": current.status.as_str() }));
    }
    MachineControl::transition(
        tx,
        tenant,
        target,
        &TargetFence {
            generation: current.generation,
        },
        TargetStatus::Ready,
    )
    .await
    .map_err(|error| rejected(&OpsError::Conflict(error.to_string())))?;
    Ok(json!({ "target_status": { "from": "DRAINING", "to": "READY" } }))
}

async fn load_target(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    tenant: &str,
    target: &str,
) -> Result<quansio_machine::control::ExecutionTarget, EventError> {
    MachineControl::load(tx, tenant, target)
        .await
        .map_err(|error| {
            rejected(&OpsError::NotFound(format!(
                "execution target {target}: {error}"
            )))
        })
}

/// `docs/runbooks/kill-switches.md`, rendered from the switch catalog.
#[must_use]
pub fn runbook_markdown() -> String {
    let mut out = String::from(
        "# Kill switches (OPS-008)\n\n\
         <!-- GENERATED from crates/server/src/control/ops/mod.rs by runbook_markdown(); \
         regenerate with QUANSIO_WRITE_RUNBOOKS=1 cargo test -p quansio-server --test ops_kill_switches. -->\n\n\
         Not authority: DOSSIER.md §21.5 and DOMAIN.md §7.6 are. Every switch is a `runtime.control` \
         action (tier 3, tenant owners and administrators), audited on engage and release, emitted as \
         `ops.kill_switch_engaged` / `ops.kill_switch_released`, and reversible: releasing restores \
         service and nothing is deleted. `GET /v1/ops/switches` shows what is engaged, by whom and why.\n\n\
         Generic commands for every switch: `EngageKillSwitch {switch, target, reason}` and \
         `ReleaseKillSwitch {switch, target, reason?}`.\n",
    );
    for (index, switch) in KillSwitch::ALL.iter().enumerate() {
        let spec = switch.spec();
        out.push_str(&format!(
            "\n## {}. `{}`\n\n- **Target:** {}\n- **Engage:** `{}`\n- **While engaged:** {}\n- **Untouched:** {}\n- **Release:** `{}`\n- **Recovery:** {}\n",
            index + 1,
            switch.as_str(),
            spec.target,
            spec.engage_command,
            spec.effect,
            spec.keeps,
            spec.release_command,
            spec.recovery,
        ));
    }
    out.push_str(
        "\n## Incident drill\n\n\
         1. Engage the narrowest switch that stops the harm (a workspace freeze before a tenant freeze; \
         one provider before an emergency deny), with a reason that names the incident.\n\
         2. Confirm it in `GET /v1/ops/switches` and in `GET /v1/audit`.\n\
         3. Fix the cause. Effects left `OUTCOME_UNKNOWN` reconcile; they are never re-dispatched.\n\
         4. Release with a reason, and confirm the release event and audit entry.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_checked_per_switch() {
        assert!(check_target(KillSwitch::EffectFreeze, "*").is_ok());
        assert!(check_target(KillSwitch::EffectFreeze, "ws_01J8Z3K6F1N8VQ2X5W9Y0RR011").is_ok());
        assert!(check_target(KillSwitch::EffectFreeze, "tgt_01J8Z3K6F1N8VQ2X5W9Y0RR011").is_err());
        assert!(check_target(KillSwitch::StreamShed, "ws_x").is_err());
        assert!(check_target(KillSwitch::ProviderDisable, "anthropic").is_ok());
        assert!(check_target(KillSwitch::ProviderDisable, "Anthropic; drop").is_err());
        assert!(check_target(KillSwitch::PolicyEmergencyDeny, "message.send").is_ok());
        assert!(check_target(KillSwitch::PolicyEmergencyDeny, "scm.*").is_ok());
        assert!(check_target(KillSwitch::PolicyEmergencyDeny, "*").is_ok());
        assert!(check_target(KillSwitch::PolicyEmergencyDeny, "a b").is_err());
        assert!(check_target(KillSwitch::WorkerQuarantine, "*").is_err());
    }

    #[test]
    fn the_gate_reads_each_switch_as_its_owner_needs_it() {
        let gate = OpsGate::from_rows(vec![
            ("effect.freeze".into(), "ws_A".into()),
            ("policy.emergency_deny".into(), "scm.*".into()),
            ("policy.emergency_deny".into(), "message.send".into()),
            ("worker.quarantine".into(), "tgt_Q".into()),
            ("provider.disable".into(), "openai".into()),
            ("provider.disable".into(), "anthropic".into()),
            ("stream.shed".into(), "*".into()),
        ]);
        assert!(gate.freezes("ws_A", 2) && gate.freezes("ws_A", 4));
        assert!(!gate.freezes("ws_A", 1), "tier 0/1 continue");
        assert!(!gate.freezes("ws_B", 3), "another workspace");
        assert!(OpsGate::from_rows(vec![("effect.freeze".into(), "*".into())]).freezes("ws_B", 2));
        assert_eq!(gate.emergency_deny("scm.remote.write"), Some("scm.*"));
        assert_eq!(gate.emergency_deny("message.send"), Some("message.send"));
        assert_eq!(gate.emergency_deny("messages.send"), None);
        assert_eq!(gate.emergency_deny("scmx.write"), None);
        assert!(gate.quarantines("tgt_Q") && !gate.quarantines("tgt_R"));
        assert_eq!(
            gate.disabled_providers(),
            ["anthropic".to_string(), "openai".to_string()]
        );
        assert!(gate.shedding());
    }

    #[test]
    fn every_switch_has_a_runbook_entry() {
        let runbook = runbook_markdown();
        for switch in KillSwitch::ALL {
            assert!(runbook.contains(&format!("`{}`", switch.as_str())));
            assert_eq!(KillSwitch::parse(switch.as_str()), Some(switch));
        }
    }
}
