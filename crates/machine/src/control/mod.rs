//! Execution-target and lease lifecycle — the machine-control authority (EXEC-001, DOMAIN.md §8.1–§8.3).
//!
//! Every execution environment the platform runs work in — a long-lived workspace computer, a disposable
//! task runtime, a local capsule, a Windows VM, a customer's private worker — is one [`ExecutionTarget`]
//! with a class, a substrate, a status and a generation. This module owns that lifecycle and the
//! [`Lease`] a controller must hold to drive a target, and it is the *only* writer of both: a second
//! mutator would be a second machine-control authority, which is what acceptance 1 forbids.
//!
//! Two fences keep a stale controller harmless:
//!
//! * **the generation.** A target's `generation` is the controller epoch. A transition is guarded on the
//!   generation the caller holds, so a controller that was replaced cannot steer the target it used to
//!   own; the generation is bumped when a target is replaced and whenever a lease expires.
//! * **the lease.** A lease is `(id, holder, generation, expires_at, status)`. Acquiring one is refused
//!   while another *live* lease is held (the row is locked first, so two racing acquirers cannot both
//!   win), and [`MachineControl::validate_lease`] is the check a worker makes before it acts: an action
//!   whose `(lease_id, generation)` does not match a live lease is refused, which is what stops a
//!   disconnected worker from finishing an action after its lease expired or was revoked.
//!
//! Nothing here decides *what* to run; placement, scheduling and dispatch are the runtime's (RUN-005,
//! RUN-011). This module decides whether a target may be steered, by whom, and until when.

use sqlx::postgres::PgConnection;
use sqlx::Row;

/// Refusal rules, named so a caller can tell which one fired.
pub mod rules {
    /// The target does not exist for this tenant.
    pub const NOT_FOUND: &str = "machine.not_found";
    /// The target is not in a state that allows the requested transition.
    pub const ILLEGAL_TRANSITION: &str = "machine.illegal_transition";
    /// The caller's generation is not the target's current one.
    pub const STALE_GENERATION: &str = "machine.stale_generation";
    /// Another live lease is already held.
    pub const LEASE_HELD: &str = "machine.lease_held";
    /// The lease does not permit the action: wrong generation, not held, or expired.
    pub const LEASE_FENCED: &str = "machine.lease_fenced";
    /// The target is not ready to be leased.
    pub const TARGET_NOT_READY: &str = "machine.target_not_ready";
    /// The requested value is not one the schema's vocabulary admits.
    pub const UNKNOWN_VALUE: &str = "machine.unknown_value";
}

/// What a target is for (DOMAIN.md §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetClass {
    /// Durable environment per workspace, shared by its teammates and checkpointed.
    PersistentWorkspaceComputer,
    /// Disposable environment for one run or work node.
    IsolatedTaskRuntime,
}

impl TargetClass {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersistentWorkspaceComputer => "persistent_workspace_computer",
            Self::IsolatedTaskRuntime => "isolated_task_runtime",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "persistent_workspace_computer" => Some(Self::PersistentWorkspaceComputer),
            "isolated_task_runtime" => Some(Self::IsolatedTaskRuntime),
            _ => None,
        }
    }
}

/// Where a target runs (DOMAIN.md §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Substrate {
    /// Managed cloud, Firecracker microVM (the default).
    CloudMicrovm,
    /// The user's Mac, a Linux guest on Virtualization.framework.
    LocalCapsuleMacos,
    /// The user's PC, a WSL2 Linux guest.
    LocalCapsuleWindows,
    /// Windows VM or host reached through the native broker.
    WindowsNative,
    /// A customer's VPC or on-prem host running qworkerd, outbound control only.
    CustomerPrivateWorker,
}

impl Substrate {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CloudMicrovm => "cloud_microvm",
            Self::LocalCapsuleMacos => "local_capsule_macos",
            Self::LocalCapsuleWindows => "local_capsule_windows",
            Self::WindowsNative => "windows_native",
            Self::CustomerPrivateWorker => "customer_private_worker",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cloud_microvm" => Some(Self::CloudMicrovm),
            "local_capsule_macos" => Some(Self::LocalCapsuleMacos),
            "local_capsule_windows" => Some(Self::LocalCapsuleWindows),
            "windows_native" => Some(Self::WindowsNative),
            "customer_private_worker" => Some(Self::CustomerPrivateWorker),
            _ => None,
        }
    }

    /// Whether this substrate is controlled outbound-only (a private worker dials in; it is never
    /// dialled), which is the property the gateway's control channel must preserve.
    #[must_use]
    pub const fn is_outbound_only(self) -> bool {
        matches!(self, Self::CustomerPrivateWorker)
    }
}

/// A target's lifecycle state (DOMAIN.md §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetStatus {
    /// Asked for, not yet being built.
    Requested,
    /// Being provisioned.
    Provisioning,
    /// Ready to be leased.
    Ready,
    /// Running work under a lease.
    Busy,
    /// Work is finishing; no new leases.
    Draining,
    /// Stopped and idle.
    Stopped,
    /// Snapshot taken; still restorable.
    Snapshotted,
    /// Gone. Terminal.
    Destroyed,
    /// Failed with a typed reason.
    Failed,
    /// Being replaced after a failure.
    Replacing,
}

impl TargetStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "REQUESTED",
            Self::Provisioning => "PROVISIONING",
            Self::Ready => "READY",
            Self::Busy => "BUSY",
            Self::Draining => "DRAINING",
            Self::Stopped => "STOPPED",
            Self::Snapshotted => "SNAPSHOTTED",
            Self::Destroyed => "DESTROYED",
            Self::Failed => "FAILED",
            Self::Replacing => "REPLACING",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "REQUESTED" => Some(Self::Requested),
            "PROVISIONING" => Some(Self::Provisioning),
            "READY" => Some(Self::Ready),
            "BUSY" => Some(Self::Busy),
            "DRAINING" => Some(Self::Draining),
            "STOPPED" => Some(Self::Stopped),
            "SNAPSHOTTED" => Some(Self::Snapshotted),
            "DESTROYED" => Some(Self::Destroyed),
            "FAILED" => Some(Self::Failed),
            "REPLACING" => Some(Self::Replacing),
            _ => None,
        }
    }

    /// Whether no further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Destroyed)
    }

    /// Whether a lease may be acquired while the target is in this state.
    #[must_use]
    pub const fn is_leasable(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Whether the transition `self -> to` is one DOMAIN.md §8.2 draws.
    ///
    /// The diagram is `REQUESTED → PROVISIONING → READY ⇄ BUSY`, `READY → DRAINING → STOPPED →
    /// (SNAPSHOTTED) → DESTROYED`, and `any → FAILED → REPLACING → PROVISIONING`. Two edges the diagram
    /// leaves implicit are admitted and named here: `STOPPED → DESTROYED` (a target stopped without a
    /// snapshot is still destroyed) and `SNAPSHOTTED → READY` (a restorable target is brought back rather
    /// than cloned). Everything else is refused — a lifecycle that accepts anything is not a lifecycle.
    #[must_use]
    pub const fn allows(self, to: Self) -> bool {
        if self.is_terminal() {
            return false;
        }
        if matches!(to, Self::Failed) {
            // Any live state may fail.
            return true;
        }
        match self {
            Self::Requested => matches!(to, Self::Provisioning),
            Self::Provisioning => matches!(to, Self::Ready),
            Self::Ready => matches!(to, Self::Busy | Self::Draining),
            Self::Busy => matches!(to, Self::Ready | Self::Draining),
            Self::Draining => matches!(to, Self::Stopped),
            Self::Stopped => matches!(to, Self::Snapshotted | Self::Destroyed),
            Self::Snapshotted => matches!(to, Self::Destroyed | Self::Ready),
            Self::Failed => matches!(to, Self::Replacing | Self::Destroyed),
            Self::Replacing => matches!(to, Self::Provisioning),
            Self::Destroyed => false,
        }
    }
}

/// A lease's lifecycle state (DOMAIN.md §8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseStatus {
    /// The holder may steer the target until `expires_at`.
    Held,
    /// Given up by its holder.
    Released,
    /// Timed out; the target's generation was bumped when it was.
    Expired,
    /// Taken away, for example because the holder was fenced.
    Revoked,
}

impl LeaseStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Released => "released",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "held" => Some(Self::Held),
            "released" => Some(Self::Released),
            "expired" => Some(Self::Expired),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

/// A target's health, derived from what the platform observes (DOMAIN.md §8.2).
///
/// Health is *derived*, never reported by the target: a worker's own claim about itself is not
/// authority. What can be checked is whether it is still beating and whether what it observes matches
/// what was asked of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetHealth {
    /// No heartbeat has ever been recorded.
    Unknown,
    /// The last heartbeat is older than the staleness window.
    Stale,
    /// Beating, and its observed state matches the desired one (or none is desired).
    Healthy,
    /// Beating, but the observed state is not the desired one.
    Diverged,
}

impl TargetHealth {
    /// Canonical value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Stale => "stale",
            Self::Healthy => "healthy",
            Self::Diverged => "diverged",
        }
    }
}

/// One execution target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTarget {
    /// Target id (`tgt_…`).
    pub id: String,
    /// Workspace that owns it.
    pub workspace_id: String,
    /// What it is for.
    pub class: TargetClass,
    /// Where it runs.
    pub substrate: Substrate,
    /// Lifecycle state.
    pub status: TargetStatus,
    /// Controller epoch; a stale controller's generation no longer matches.
    pub generation: i64,
    /// Lease currently attached to the target, when one is live.
    pub lease_id: Option<String>,
    /// Image the target was provisioned from.
    pub image_digest: Option<String>,
    /// Last heartbeat a worker reported.
    pub last_heartbeat_at: Option<String>,
    /// What the platform asked the target to be.
    pub desired_state: Option<String>,
    /// What the target reported it is.
    pub observed_state: Option<String>,
}

impl ExecutionTarget {
    /// The target's health, from the heartbeat freshness the store established and the two states.
    ///
    /// The freshness comparison is deliberately *not* done here: a heartbeat window is an interval, and
    /// intervals belong to the database that stores the instants. The store asks whether the heartbeat is
    /// stale and this function classifies the answer, so there is one piece of interval arithmetic in the
    /// module rather than two that could disagree.
    #[must_use]
    pub fn classify_health(
        heartbeat_at: Option<&str>,
        stale: bool,
        desired_state: Option<&str>,
        observed_state: Option<&str>,
    ) -> TargetHealth {
        if heartbeat_at.is_none() {
            return TargetHealth::Unknown;
        }
        if stale {
            return TargetHealth::Stale;
        }
        match (desired_state, observed_state) {
            (Some(desired), Some(observed)) if desired != observed => TargetHealth::Diverged,
            _ => TargetHealth::Healthy,
        }
    }

    /// This target's health, given whether its heartbeat is stale.
    #[must_use]
    pub fn health(&self, stale: bool) -> TargetHealth {
        Self::classify_health(
            self.last_heartbeat_at.as_deref(),
            stale,
            self.desired_state.as_deref(),
            self.observed_state.as_deref(),
        )
    }
}

/// Request to register a target.
#[derive(Debug, Clone)]
pub struct NewTarget {
    /// Target id.
    pub id: String,
    /// Workspace that owns it.
    pub workspace_id: String,
    /// What it is for.
    pub class: TargetClass,
    /// Where it runs.
    pub substrate: Substrate,
    /// Image to provision from.
    pub image_digest: Option<String>,
    /// What the platform asks the target to be, when it is known at registration.
    pub desired_state: Option<String>,
}

/// One lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// Lease id (`lse_…`).
    pub id: String,
    /// Target it leases.
    pub target_id: String,
    /// Controller holding it.
    pub holder_controller_id: String,
    /// Generation the holder must present with the lease.
    pub holder_generation: i64,
    /// RFC 3339 instant it expires at.
    pub expires_at: String,
    /// Lifecycle state.
    pub status: LeaseStatus,
}

/// A request to acquire a lease: the lease's identity, the controller asking, its fence and its window.
#[derive(Debug, Clone)]
pub struct LeaseRequest<'a> {
    /// Lease id to create (`lse_…`).
    pub lease_id: &'a str,
    /// Target to lease.
    pub target_id: &'a str,
    /// Controller that will hold it.
    pub controller_id: &'a str,
    /// Generation the controller holds.
    pub fence: &'a TargetFence,
    /// How long the lease lasts, in seconds.
    pub ttl_seconds: i64,
    /// The instant the lease is acquired at.
    pub now: &'a str,
}

/// What a caller holds when it asks to steer a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetFence {
    /// Generation the caller holds.
    pub generation: i64,
}

/// Machine-control errors.
#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    /// The database rejected the operation.
    #[error("machine control: {0}")]
    Database(#[from] sqlx::Error),
    /// The target does not exist for this tenant.
    #[error("execution target {0} not found")]
    TargetNotFound(String),
    /// The lease does not exist for this tenant.
    #[error("lease {0} not found")]
    LeaseNotFound(String),
    /// The transition is not one the lifecycle draws.
    #[error("target {id} cannot move from {from} to {to}")]
    IllegalTransition {
        /// Target id.
        id: String,
        /// Current state.
        from: &'static str,
        /// Requested state.
        to: &'static str,
    },
    /// The caller's generation is stale.
    #[error("target {id} is at generation {current}, the caller holds {held}")]
    StaleGeneration {
        /// Target id.
        id: String,
        /// Generation recorded.
        current: i64,
        /// Generation the caller holds.
        held: i64,
    },
    /// Another live lease is held.
    #[error("target {id} is leased by {holder} until {until}")]
    LeaseHeld {
        /// Target id.
        id: String,
        /// Current holder.
        holder: String,
        /// When its lease expires.
        until: String,
    },
    /// The target is not in a leasable state.
    #[error("target {id} is {status}, not ready to be leased")]
    TargetNotReady {
        /// Target id.
        id: String,
        /// Its state.
        status: &'static str,
    },
    /// The lease does not permit the action.
    #[error("lease {id} does not permit this action: {reason}")]
    LeaseFenced {
        /// Lease id.
        id: String,
        /// Why it does not.
        reason: &'static str,
    },
    /// A stored value is not one the schema admits.
    #[error("unknown {field}: {value}")]
    UnknownValue {
        /// Which column.
        field: &'static str,
        /// The value found.
        value: String,
    },
}

const COLUMNS: &str = "id, workspace_id, target_class, substrate, status, generation, lease_id, \
                       image_digest, desired_state, observed_state, \
                       to_char(last_heartbeat_at AT TIME ZONE 'UTC', \
                       'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS last_heartbeat_at";

/// The instant columns are rendered in the same shape a caller passes in (ISO-8601 UTC, `Z`), so a
/// returned lease or target compares with the caller's own clock instead of with Postgres's rendering.
const LEASE_COLUMNS: &str = "id, target_id, holder_controller_id, holder_generation, \
     to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at, status";

const INSTANT_ADD: &str =
    "to_char(($1::timestamptz + make_interval(secs => $2)) AT TIME ZONE 'UTC', \
     'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at";

/// Durable machine-control store: the only writer of targets and leases.
pub struct MachineControl;

impl MachineControl {
    /// Establish the tenant context the table's forced row-level security requires.
    async fn tenant(conn: &mut PgConnection, tenant_id: &str) -> Result<(), MachineError> {
        sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
            .bind(tenant_id)
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// Register a target in `REQUESTED` at generation 1.
    ///
    /// # Errors
    /// Returns a database error when the write fails.
    pub async fn register(
        conn: &mut PgConnection,
        tenant_id: &str,
        target: &NewTarget,
    ) -> Result<(), MachineError> {
        Self::tenant(conn, tenant_id).await?;
        sqlx::query(
            "INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, \
             status, generation, image_digest, desired_state) \
             VALUES ($1, $2, $3, $4, $5, 'REQUESTED', 1, $6, $7)",
        )
        .bind(&target.id)
        .bind(tenant_id)
        .bind(&target.workspace_id)
        .bind(target.class.as_str())
        .bind(target.substrate.as_str())
        .bind(&target.image_digest)
        .bind(&target.desired_state)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Bind a target to a network policy.
    ///
    /// A target with no policy reaches nothing (EXEC-008's deny by default), so this is the
    /// operation that makes a target's egress governable at all. It writes only `network_policy_id`,
    /// which DOMAIN.md §8.2 already defines on the target, and it does not move the generation: the
    /// policy's own revision is what fences the grants issued under it, so a rebind does not have to
    /// disturb a controller that holds a lease.
    ///
    /// # Errors
    /// Returns [`MachineError::TargetNotFound`] when the tenant has no such target.
    pub async fn set_network_policy(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        policy_id: Option<&str>,
    ) -> Result<(), MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let updated = sqlx::query(
            "UPDATE execution_targets SET network_policy_id = $3 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .bind(policy_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(MachineError::TargetNotFound(target_id.to_string()));
        }
        Ok(())
    }

    /// Load one target.
    ///
    /// # Errors
    /// Returns [`MachineError::TargetNotFound`] when the tenant has no such target.
    pub async fn load(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
    ) -> Result<ExecutionTarget, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let sql =
            format!("SELECT {COLUMNS} FROM execution_targets WHERE id = $1 AND tenant_id = $2");
        let row = sqlx::query(&sql)
            .bind(target_id)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| MachineError::TargetNotFound(target_id.to_string()))?;
        Self::row_to_target(&row)
    }

    /// Every target of a workspace, newest first.
    ///
    /// # Errors
    /// Returns a database error when the query fails.
    pub async fn list_for_workspace(
        conn: &mut PgConnection,
        tenant_id: &str,
        workspace_id: &str,
    ) -> Result<Vec<ExecutionTarget>, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let sql = format!(
            "SELECT {COLUMNS} FROM execution_targets WHERE workspace_id = $1 AND tenant_id = $2 \
             ORDER BY created_at DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(workspace_id)
            .bind(tenant_id)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter().map(Self::row_to_target).collect()
    }

    /// Move a target along its lifecycle, if the caller still holds its generation.
    ///
    /// The row is read `FOR UPDATE` so the fence check and the write share one unit of work: a
    /// concurrent replacement cannot land between them.
    ///
    /// # Errors
    /// Returns [`MachineError::StaleGeneration`] when the caller's fence is old,
    /// [`MachineError::IllegalTransition`] when the lifecycle does not draw the edge, and
    /// [`MachineError::TargetNotFound`] when the target does not exist.
    pub async fn transition(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        fence: &TargetFence,
        to: TargetStatus,
    ) -> Result<ExecutionTarget, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let sql = format!(
            "SELECT {COLUMNS} FROM execution_targets WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
        );
        let row = sqlx::query(&sql)
            .bind(target_id)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| MachineError::TargetNotFound(target_id.to_string()))?;
        let target = Self::row_to_target(&row)?;

        if target.generation != fence.generation {
            return Err(MachineError::StaleGeneration {
                id: target.id,
                current: target.generation,
                held: fence.generation,
            });
        }
        if !target.status.allows(to) {
            return Err(MachineError::IllegalTransition {
                id: target.id,
                from: target.status.as_str(),
                to: to.as_str(),
            });
        }
        // Replacing hands the target to a new controller, so the generation moves with it.
        let generation = if to == TargetStatus::Replacing {
            target.generation + 1
        } else {
            target.generation
        };
        sqlx::query(
            "UPDATE execution_targets SET status = $3, generation = $4 WHERE id = $1 AND tenant_id = $2",
        )
            .bind(&target.id)
            .bind(tenant_id)
            .bind(to.as_str())
            .bind(generation)
            .execute(&mut *conn)
            .await?;
        Ok(ExecutionTarget {
            status: to,
            generation,
            ..target
        })
    }

    /// Acquire a lease on a ready target, unless another live one is held.
    ///
    /// # Errors
    /// Returns [`MachineError::TargetNotReady`] when the target is not `READY`,
    /// [`MachineError::StaleGeneration`] when the caller's fence is old, and
    /// [`MachineError::LeaseHeld`] when another controller holds a live lease.
    pub async fn acquire_lease(
        conn: &mut PgConnection,
        tenant_id: &str,
        request: &LeaseRequest<'_>,
    ) -> Result<Lease, MachineError> {
        let LeaseRequest {
            lease_id,
            target_id,
            controller_id,
            fence,
            ttl_seconds,
            now,
        } = *request;
        Self::tenant(conn, tenant_id).await?;
        let sql = format!(
            "SELECT {COLUMNS} FROM execution_targets WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
        );
        let row = sqlx::query(&sql)
            .bind(target_id)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| MachineError::TargetNotFound(target_id.to_string()))?;
        let target = Self::row_to_target(&row)?;

        if target.generation != fence.generation {
            return Err(MachineError::StaleGeneration {
                id: target.id,
                current: target.generation,
                held: fence.generation,
            });
        }
        if !target.status.is_leasable() {
            return Err(MachineError::TargetNotReady {
                id: target.id,
                status: target.status.as_str(),
            });
        }
        if let Some(existing) = Self::live_lease(conn, tenant_id, target_id, now).await? {
            return Err(MachineError::LeaseHeld {
                id: target.id,
                holder: existing.holder_controller_id,
                until: existing.expires_at,
            });
        }

        let expires_at: String = sqlx::query(&format!("SELECT {INSTANT_ADD}"))
            .bind(now)
            .bind(ttl_seconds as f64)
            .fetch_one(&mut *conn)
            .await?
            .try_get("expires_at")?;

        sqlx::query(
            "INSERT INTO leases (id, tenant_id, target_id, holder_controller_id, holder_generation, \
             expires_at, status) VALUES ($1, $2, $3, $4, $5, $6::timestamptz, 'held')",
        )
        .bind(lease_id)
        .bind(tenant_id)
        .bind(target_id)
        .bind(controller_id)
        .bind(fence.generation)
        .bind(&expires_at)
        .execute(&mut *conn)
        .await?;
        sqlx::query("UPDATE execution_targets SET lease_id = $3 WHERE id = $1 AND tenant_id = $2")
            .bind(target_id)
            .bind(tenant_id)
            .bind(lease_id)
            .execute(&mut *conn)
            .await?;

        Ok(Lease {
            id: lease_id.to_string(),
            target_id: target_id.to_string(),
            holder_controller_id: controller_id.to_string(),
            holder_generation: fence.generation,
            expires_at,
            status: LeaseStatus::Held,
        })
    }

    /// Renew a held lease, moving its expiry out by `ttl_seconds`.
    ///
    /// # Errors
    /// Returns [`MachineError::LeaseFenced`] when the lease is not held or its generation does not
    /// match, and [`MachineError::LeaseNotFound`] when it does not exist.
    pub async fn renew_lease(
        conn: &mut PgConnection,
        tenant_id: &str,
        lease_id: &str,
        generation: i64,
        ttl_seconds: i64,
        now: &str,
    ) -> Result<Lease, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let lease = Self::load_lease(conn, tenant_id, lease_id).await?;
        if lease.status != LeaseStatus::Held {
            return Err(MachineError::LeaseFenced {
                id: lease.id,
                reason: "the lease is not held",
            });
        }
        if lease.holder_generation != generation {
            return Err(MachineError::LeaseFenced {
                id: lease.id,
                reason: "the lease belongs to another generation",
            });
        }
        let expires_at: String = sqlx::query(&format!("SELECT {INSTANT_ADD}"))
            .bind(now)
            .bind(ttl_seconds as f64)
            .fetch_one(&mut *conn)
            .await?
            .try_get("expires_at")?;
        sqlx::query(
            "UPDATE leases SET expires_at = $3::timestamptz, renewed_at = $4::timestamptz \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(lease_id)
        .bind(tenant_id)
        .bind(&expires_at)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        Ok(Lease {
            expires_at,
            ..lease
        })
    }

    /// Release a held lease.
    ///
    /// # Errors
    /// Returns [`MachineError::LeaseFenced`] when the lease is not held and
    /// [`MachineError::LeaseNotFound`] when it does not exist.
    pub async fn release_lease(
        conn: &mut PgConnection,
        tenant_id: &str,
        lease_id: &str,
    ) -> Result<(), MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let updated = sqlx::query(
            "UPDATE leases SET status = 'released' WHERE id = $1 AND tenant_id = $2 AND status = 'held'",
        )
            .bind(lease_id)
            .bind(tenant_id)
            .execute(&mut *conn)
            .await?
            .rows_affected();
        if updated == 0 {
            let lease = Self::load_lease(conn, tenant_id, lease_id).await?;
            return Err(MachineError::LeaseFenced {
                id: lease.id,
                reason: "the lease is not held",
            });
        }
        sqlx::query(
            "UPDATE execution_targets SET lease_id = NULL WHERE tenant_id = $2 AND lease_id = $1",
        )
        .bind(lease_id)
        .bind(tenant_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Revoke a lease and bump the target's generation, fencing whoever held it.
    ///
    /// # Errors
    /// Returns [`MachineError::LeaseNotFound`] when the lease does not exist.
    pub async fn revoke_lease(
        conn: &mut PgConnection,
        tenant_id: &str,
        lease_id: &str,
    ) -> Result<(), MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let lease = Self::load_lease(conn, tenant_id, lease_id).await?;
        sqlx::query("UPDATE leases SET status = 'revoked' WHERE id = $1 AND tenant_id = $2")
            .bind(lease_id)
            .bind(tenant_id)
            .execute(&mut *conn)
            .await?;
        Self::bump_generation(conn, tenant_id, &lease.target_id).await?;
        sqlx::query(
            "UPDATE execution_targets SET lease_id = NULL WHERE tenant_id = $2 AND lease_id = $1",
        )
        .bind(lease_id)
        .bind(tenant_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Check that a lease permits an action right now — the fence a worker applies before it acts.
    ///
    /// # Errors
    /// Returns [`MachineError::LeaseFenced`] when the lease is not live, was not presented at the
    /// generation it was granted for, or has expired; [`MachineError::LeaseNotFound`] when it does not
    /// exist. A worker that cannot pass this check must not execute the action.
    pub async fn validate_lease(
        conn: &mut PgConnection,
        tenant_id: &str,
        lease_id: &str,
        generation: i64,
        now: &str,
    ) -> Result<Lease, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let lease = sqlx::query(
            "SELECT l.id, l.target_id, l.holder_controller_id, l.holder_generation, \
             to_char(l.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at, \
             l.status, (l.expires_at > $3::timestamptz) AS live \
             FROM leases l WHERE l.id = $1 AND l.tenant_id = $2",
        )
        .bind(lease_id)
        .bind(tenant_id)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| MachineError::LeaseNotFound(lease_id.to_string()))?;
        let status: String = lease.try_get("status")?;
        let status = LeaseStatus::parse(&status).ok_or_else(|| MachineError::UnknownValue {
            field: "leases.status",
            value: status,
        })?;
        if status != LeaseStatus::Held {
            return Err(MachineError::LeaseFenced {
                id: lease_id.to_string(),
                reason: "the lease is not held",
            });
        }
        let holder_generation: i64 = lease.try_get("holder_generation")?;
        if holder_generation != generation {
            return Err(MachineError::LeaseFenced {
                id: lease_id.to_string(),
                reason: "the lease was granted to another generation",
            });
        }
        let live: bool = lease.try_get("live")?;
        if !live {
            return Err(MachineError::LeaseFenced {
                id: lease_id.to_string(),
                reason: "the lease has expired",
            });
        }
        Ok(Lease {
            id: lease.try_get("id")?,
            target_id: lease.try_get("target_id")?,
            holder_controller_id: lease.try_get("holder_controller_id")?,
            holder_generation,
            expires_at: lease.try_get("expires_at")?,
            status,
        })
    }

    /// Expire every lease that is past its expiry, fencing its holder.
    ///
    /// Each expired lease's target has its generation bumped, so a controller that reconnects holding
    /// the old generation finds its transitions and acquisitions refused: the lease expiring is what
    /// fences it, and it is fenced without the target having to be told who held it.
    ///
    /// # Errors
    /// Returns a database error when the update fails.
    pub async fn expire_leases(
        conn: &mut PgConnection,
        tenant_id: &str,
        now: &str,
    ) -> Result<Vec<Lease>, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let rows = sqlx::query(
            "UPDATE leases SET status = 'expired' \
             WHERE tenant_id = $1 AND status = 'held' AND expires_at <= $2::timestamptz \
             RETURNING id, target_id, holder_controller_id, holder_generation, \
             to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at",
        )
        .bind(tenant_id)
        .bind(now)
        .fetch_all(&mut *conn)
        .await?;

        let mut expired = Vec::with_capacity(rows.len());
        for row in rows {
            let target_id: String = row.try_get("target_id")?;
            Self::bump_generation(conn, tenant_id, &target_id).await?;
            sqlx::query(
                "UPDATE execution_targets SET lease_id = NULL WHERE id = $1 AND tenant_id = $2",
            )
            .bind(&target_id)
            .bind(tenant_id)
            .execute(&mut *conn)
            .await?;
            expired.push(Lease {
                id: row.try_get("id")?,
                target_id,
                holder_controller_id: row.try_get("holder_controller_id")?,
                holder_generation: row.try_get("holder_generation")?,
                expires_at: row.try_get("expires_at")?,
                status: LeaseStatus::Expired,
            });
        }
        Ok(expired)
    }

    /// Record what the platform asks a target to be.
    ///
    /// # Errors
    /// Returns a database error when the write fails, and
    /// [`MachineError::TargetNotFound`] when the tenant has no such target.
    pub async fn set_desired_state(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        desired_state: &str,
    ) -> Result<ExecutionTarget, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let updated = sqlx::query(
            "UPDATE execution_targets SET desired_state = $3 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .bind(desired_state)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(MachineError::TargetNotFound(target_id.to_string()));
        }
        Self::load(conn, tenant_id, target_id).await
    }

    /// Record a heartbeat: what the target reports it is, and when.
    ///
    /// Health is derived from this (see [`ExecutionTarget::health`]) rather than taken from the target's
    /// own claim, so an observation is stored as data and classified by the platform.
    ///
    /// # Errors
    /// Returns a database error when the write fails, and
    /// [`MachineError::TargetNotFound`] when the tenant has no such target.
    pub async fn observe(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        observed_state: &str,
        at: &str,
    ) -> Result<ExecutionTarget, MachineError> {
        Self::tenant(conn, tenant_id).await?;
        let updated = sqlx::query(
            "UPDATE execution_targets SET observed_state = $3, last_heartbeat_at = $4::timestamptz \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .bind(observed_state)
        .bind(at)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(MachineError::TargetNotFound(target_id.to_string()));
        }
        Self::load(conn, tenant_id, target_id).await
    }

    async fn bump_generation(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
    ) -> Result<(), MachineError> {
        sqlx::query(
            "UPDATE execution_targets SET generation = generation + 1 WHERE id = $1 AND tenant_id = $2",
        )
            .bind(target_id)
            .bind(tenant_id)
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    async fn live_lease(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        now: &str,
    ) -> Result<Option<Lease>, MachineError> {
        let sql = format!(
            "SELECT {LEASE_COLUMNS} FROM leases \
             WHERE target_id = $1 AND tenant_id = $2 AND status = 'held' AND expires_at > $3::timestamptz \
             ORDER BY acquired_at DESC LIMIT 1"
        );
        let row = sqlx::query(&sql)
            .bind(target_id)
            .bind(tenant_id)
            .bind(now)
            .fetch_optional(&mut *conn)
            .await?;
        row.map(|row| {
            let status: String = row.try_get("status")?;
            let status = LeaseStatus::parse(&status).ok_or_else(|| MachineError::UnknownValue {
                field: "leases.status",
                value: status,
            })?;
            Ok(Lease {
                id: row.try_get("id")?,
                target_id: row.try_get("target_id")?,
                holder_controller_id: row.try_get("holder_controller_id")?,
                holder_generation: row.try_get("holder_generation")?,
                expires_at: row.try_get("expires_at")?,
                status,
            })
        })
        .transpose()
    }

    async fn load_lease(
        conn: &mut PgConnection,
        tenant_id: &str,
        lease_id: &str,
    ) -> Result<Lease, MachineError> {
        let sql = format!("SELECT {LEASE_COLUMNS} FROM leases WHERE id = $1 AND tenant_id = $2");
        let row = sqlx::query(&sql)
            .bind(lease_id)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| MachineError::LeaseNotFound(lease_id.to_string()))?;
        let status: String = row.try_get("status")?;
        let status = LeaseStatus::parse(&status).ok_or_else(|| MachineError::UnknownValue {
            field: "leases.status",
            value: status,
        })?;
        Ok(Lease {
            id: row.try_get("id")?,
            target_id: row.try_get("target_id")?,
            holder_controller_id: row.try_get("holder_controller_id")?,
            holder_generation: row.try_get("holder_generation")?,
            expires_at: row.try_get("expires_at")?,
            status,
        })
    }

    fn row_to_target(row: &sqlx::postgres::PgRow) -> Result<ExecutionTarget, MachineError> {
        let class: String = row.try_get("target_class")?;
        let substrate: String = row.try_get("substrate")?;
        let status: String = row.try_get("status")?;
        Ok(ExecutionTarget {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            class: TargetClass::parse(&class).ok_or_else(|| MachineError::UnknownValue {
                field: "execution_targets.target_class",
                value: class,
            })?,
            substrate: Substrate::parse(&substrate).ok_or_else(|| MachineError::UnknownValue {
                field: "execution_targets.substrate",
                value: substrate,
            })?,
            status: TargetStatus::parse(&status).ok_or_else(|| MachineError::UnknownValue {
                field: "execution_targets.status",
                value: status,
            })?,
            generation: row.try_get("generation")?,
            lease_id: row.try_get("lease_id")?,
            image_digest: row.try_get("image_digest")?,
            last_heartbeat_at: row.try_get("last_heartbeat_at")?,
            desired_state: row.try_get("desired_state")?,
            observed_state: row.try_get("observed_state")?,
        })
    }
}
