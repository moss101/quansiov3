//! Durable native-computer ownership and input fencing (DOMAIN.md §8.6).

use sqlx::{Acquire, PgConnection, Row};

use super::MachineHolder;

/// One execution target's native-computer control state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerControl {
    /// ExecutionTarget identity; also the row identity.
    pub target_id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Run currently using the target, when one is attached.
    pub run_id: Option<String>,
    /// Who may produce input.
    pub holder: MachineHolder,
    /// Monotonic fence incremented whenever control changes hands.
    pub generation: i64,
    /// Whether a takeover has fenced new input and is draining the active call.
    pub takeover_pending: bool,
    /// Exact ToolCall currently allowed to cross the bridge.
    pub active_tool_call_id: Option<String>,
    /// RFC 3339 second at which control last changed hands.
    pub control_since: Option<String>,
}

impl ComputerControl {
    /// Whether a new agent input may begin.
    #[must_use]
    pub const fn agent_may_input(&self) -> bool {
        matches!(self.holder, MachineHolder::Agent)
            && !self.takeover_pending
            && self.active_tool_call_id.is_none()
    }
}

/// A typed refusal at the durable native-computer control boundary.
#[derive(Debug, thiserror::Error)]
pub enum ComputerControlError {
    /// PostgreSQL refused or was unavailable.
    #[error("computer control database error: {0}")]
    Database(#[from] sqlx::Error),
    /// This tenant has no control row for the target.
    #[error("computer control for target {0} does not exist for this tenant")]
    NotFound(String),
    /// This tenant has no such ExecutionTarget.
    #[error("execution target {0} does not exist for this tenant")]
    TargetNotFound(String),
    /// A ToolCall identity was malformed.
    #[error("{0:?} is not a ToolCall id")]
    ToolCallIdInvalid(String),
    /// The caller observed an older or future control generation.
    #[error("computer control generation {expected} is stale; current generation is {current}")]
    StaleGeneration {
        /// Generation supplied by the caller.
        expected: i64,
        /// Generation stored by the authority.
        current: i64,
    },
    /// Input was attempted while a non-agent holder owns the target.
    #[error("native computer input is held by {0}")]
    Held(&'static str),
    /// A takeover has already fenced new input.
    #[error("a takeover is pending; new native computer input is fenced")]
    TakeoverPending,
    /// Another exact ToolCall remains active.
    #[error("native computer ToolCall {0} is still active")]
    ActionInFlight(String),
    /// Finish/recovery named a different ToolCall than the authority records.
    #[error("cannot finish ToolCall {actual}; active ToolCall is {expected}")]
    ActionMismatch {
        /// ToolCall held by the row.
        expected: String,
        /// ToolCall supplied by the caller.
        actual: String,
    },
    /// Completion was attempted without first requesting a takeover.
    #[error("no native computer takeover is pending")]
    TakeoverNotPending,
    /// A stored value is outside the domain model.
    #[error("stored computer control is invalid: {0}")]
    Corrupt(String),
}

const COLUMNS: &str = "target_id, tenant_id, run_id, holder, generation, takeover_pending, \
                       active_tool_call_id, \
                       to_char(control_since AT TIME ZONE 'UTC', \
                               'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS control_since";

/// PostgreSQL-backed native-computer control authority.
pub struct ComputerControlStore;

impl ComputerControlStore {
    /// Create the one control row for an ExecutionTarget.
    ///
    /// # Errors
    /// Returns [`ComputerControlError::TargetNotFound`] when the target is outside the tenant.
    pub async fn open(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        run_id: Option<&str>,
    ) -> Result<ComputerControl, ComputerControlError> {
        let mut tx = conn.begin().await?;
        set_tenant(&mut tx, tenant_id).await?;
        let target =
            sqlx::query("SELECT id FROM execution_targets WHERE id = $1 AND tenant_id = $2")
                .bind(target_id)
                .bind(tenant_id)
                .fetch_optional(&mut *tx)
                .await?;
        if target.is_none() {
            return Err(ComputerControlError::TargetNotFound(target_id.to_string()));
        }
        sqlx::query(
            "INSERT INTO computer_controls (target_id, tenant_id, run_id, holder, generation) \
             VALUES ($1, $2, $3, 'agent', 1)",
        )
        .bind(target_id)
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        let control = load_in(&mut tx, tenant_id, target_id, false).await?;
        tx.commit().await?;
        Ok(control)
    }

    /// Reload control state after a process or worker restart.
    ///
    /// # Errors
    /// Returns [`ComputerControlError::NotFound`] when the tenant has no row.
    pub async fn load(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
    ) -> Result<ComputerControl, ComputerControlError> {
        let mut tx = conn.begin().await?;
        set_tenant(&mut tx, tenant_id).await?;
        let control = load_in(&mut tx, tenant_id, target_id, false).await?;
        tx.commit().await?;
        Ok(control)
    }

    /// Atomically authorize one exact ToolCall to cross the native bridge.
    ///
    /// # Errors
    /// Fails closed for a stale generation, user ownership, pending takeover, or active call.
    pub async fn begin_action(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        tool_call_id: &str,
        generation: i64,
    ) -> Result<ComputerControl, ComputerControlError> {
        validate_tool_call_id(tool_call_id)?;
        let mut tx = conn.begin().await?;
        set_tenant(&mut tx, tenant_id).await?;
        let control = load_in(&mut tx, tenant_id, target_id, true).await?;
        require_generation(&control, generation)?;
        if !matches!(control.holder, MachineHolder::Agent) {
            return Err(ComputerControlError::Held(control.holder.as_str()));
        }
        if control.takeover_pending {
            return Err(ComputerControlError::TakeoverPending);
        }
        if let Some(active) = control.active_tool_call_id {
            return Err(ComputerControlError::ActionInFlight(active));
        }
        sqlx::query(
            "UPDATE computer_controls SET active_tool_call_id = $3 \
             WHERE target_id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .bind(tool_call_id)
        .execute(&mut *tx)
        .await?;
        let updated = load_in(&mut tx, tenant_id, target_id, false).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Settle or reconcile the exact active ToolCall and release the input slot.
    ///
    /// # Errors
    /// Refuses a stale generation or a different ToolCall identity.
    pub async fn finish_action(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        tool_call_id: &str,
        generation: i64,
    ) -> Result<ComputerControl, ComputerControlError> {
        validate_tool_call_id(tool_call_id)?;
        let mut tx = conn.begin().await?;
        set_tenant(&mut tx, tenant_id).await?;
        let control = load_in(&mut tx, tenant_id, target_id, true).await?;
        require_generation(&control, generation)?;
        match control.active_tool_call_id.as_deref() {
            Some(active) if active == tool_call_id => {}
            Some(active) => {
                return Err(ComputerControlError::ActionMismatch {
                    expected: active.to_string(),
                    actual: tool_call_id.to_string(),
                });
            }
            None => {
                return Err(ComputerControlError::ActionMismatch {
                    expected: "none".to_string(),
                    actual: tool_call_id.to_string(),
                });
            }
        }
        sqlx::query(
            "UPDATE computer_controls SET active_tool_call_id = NULL \
             WHERE target_id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
        let updated = load_in(&mut tx, tenant_id, target_id, false).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Fence new agent input immediately and begin draining the active call.
    ///
    /// # Errors
    /// Refuses a stale generation or a target already held by the user.
    pub async fn request_takeover(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        generation: i64,
    ) -> Result<ComputerControl, ComputerControlError> {
        let mut tx = conn.begin().await?;
        set_tenant(&mut tx, tenant_id).await?;
        let control = load_in(&mut tx, tenant_id, target_id, true).await?;
        require_generation(&control, generation)?;
        if !matches!(control.holder, MachineHolder::Agent) {
            return Err(ComputerControlError::Held(control.holder.as_str()));
        }
        if control.takeover_pending {
            return Err(ComputerControlError::TakeoverPending);
        }
        sqlx::query(
            "UPDATE computer_controls SET takeover_pending = TRUE \
             WHERE target_id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
        let updated = load_in(&mut tx, tenant_id, target_id, false).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Give control to the user after all input has drained or been reconciled.
    ///
    /// # Errors
    /// Refuses when no takeover is pending or an exact ToolCall remains active.
    pub async fn complete_takeover(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        generation: i64,
        at: &str,
    ) -> Result<ComputerControl, ComputerControlError> {
        let mut tx = conn.begin().await?;
        set_tenant(&mut tx, tenant_id).await?;
        let control = load_in(&mut tx, tenant_id, target_id, true).await?;
        require_generation(&control, generation)?;
        if !control.takeover_pending {
            return Err(ComputerControlError::TakeoverNotPending);
        }
        if let Some(active) = control.active_tool_call_id {
            return Err(ComputerControlError::ActionInFlight(active));
        }
        sqlx::query(
            "UPDATE computer_controls \
             SET holder = 'user', generation = generation + 1, takeover_pending = FALSE, \
                 control_since = $3::timestamptz \
             WHERE target_id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .bind(at)
        .execute(&mut *tx)
        .await?;
        let updated = load_in(&mut tx, tenant_id, target_id, false).await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Explicitly return user-held control to the agent.
    ///
    /// # Errors
    /// Refuses unless the user holds the exact generation.
    pub async fn handback(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        generation: i64,
        at: &str,
    ) -> Result<ComputerControl, ComputerControlError> {
        let mut tx = conn.begin().await?;
        set_tenant(&mut tx, tenant_id).await?;
        let control = load_in(&mut tx, tenant_id, target_id, true).await?;
        require_generation(&control, generation)?;
        if !matches!(control.holder, MachineHolder::User) {
            return Err(ComputerControlError::Held(control.holder.as_str()));
        }
        if let Some(active) = control.active_tool_call_id {
            return Err(ComputerControlError::ActionInFlight(active));
        }
        sqlx::query(
            "UPDATE computer_controls \
             SET holder = 'agent', generation = generation + 1, control_since = $3::timestamptz \
             WHERE target_id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .bind(at)
        .execute(&mut *tx)
        .await?;
        let updated = load_in(&mut tx, tenant_id, target_id, false).await?;
        tx.commit().await?;
        Ok(updated)
    }
}

async fn set_tenant(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
        .bind(tenant_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn load_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    target_id: &str,
    lock: bool,
) -> Result<ComputerControl, ComputerControlError> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let sql = format!(
        "SELECT {COLUMNS} FROM computer_controls \
         WHERE target_id = $1 AND tenant_id = $2{suffix}"
    );
    let row = sqlx::query(&sql)
        .bind(target_id)
        .bind(tenant_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ComputerControlError::NotFound(target_id.to_string()))?;
    decode(&row)
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<ComputerControl, ComputerControlError> {
    let holder: String = row.try_get("holder")?;
    let holder = MachineHolder::parse(&holder)
        .ok_or_else(|| ComputerControlError::Corrupt(format!("unknown holder {holder:?}")))?;
    let generation: i64 = row.try_get("generation")?;
    if generation < 1 {
        return Err(ComputerControlError::Corrupt(format!(
            "generation {generation} is below one"
        )));
    }
    Ok(ComputerControl {
        target_id: row.try_get("target_id")?,
        tenant_id: row.try_get("tenant_id")?,
        run_id: row.try_get("run_id")?,
        holder,
        generation,
        takeover_pending: row.try_get("takeover_pending")?,
        active_tool_call_id: row.try_get("active_tool_call_id")?,
        control_since: row.try_get("control_since")?,
    })
}

fn validate_tool_call_id(tool_call_id: &str) -> Result<(), ComputerControlError> {
    if tool_call_id.starts_with("tc_") {
        Ok(())
    } else {
        Err(ComputerControlError::ToolCallIdInvalid(
            tool_call_id.to_string(),
        ))
    }
}

fn require_generation(
    control: &ComputerControl,
    expected: i64,
) -> Result<(), ComputerControlError> {
    if control.generation == expected {
        Ok(())
    } else {
        Err(ComputerControlError::StaleGeneration {
            expected,
            current: control.generation,
        })
    }
}
