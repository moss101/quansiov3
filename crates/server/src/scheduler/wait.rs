//! Durable wait registry (DOMAIN.md §5.7).
//!
//! A run parked in `WAITING_*` is resumed only by the resolution that matches the wait it
//! actually registered: approvals, questions, external events, child agent threads and
//! timers each carry a `key`, and [`WaitRegistry::resolve`] removes exactly the matching
//! entry or fails closed with [`WaitError::NotRegistered`]. Resolving wait A therefore
//! can never resume run B, and an unknown key is never treated as a no-op.
//!
//! Waits live in `protocol_states.waits` (CORE-006) so they survive restart with the rest
//! of the protocol state; this module owns the append/resolve rules over that JSON column
//! and does not introduce a second wait store.

use sqlx::PgConnection;

use crate::runtime::protocol_state::{
    ProtocolState, ProtocolStateError, ProtocolStateStore, Wait, WaitKind,
};

/// Errors from the wait registry.
#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    /// The protocol-state store rejected the operation.
    #[error("protocol state: {0}")]
    ProtocolState(#[from] ProtocolStateError),
    /// The run already registered the same wait kind and key.
    #[error("run {run_id} already waits on {kind:?} {key:?}")]
    Duplicate {
        /// Run that already holds the wait.
        run_id: String,
        /// Wait kind.
        kind: WaitKind,
        /// Wait key.
        key: String,
    },
    /// The run has no matching wait, so the resolution must not resume it.
    #[error("run {run_id} is not waiting on {kind:?} {key:?}; a resolution only resumes the run that registered it")]
    NotRegistered {
        /// Run the resolution was addressed to.
        run_id: String,
        /// Wait kind that was resolved.
        kind: WaitKind,
        /// Wait key that was resolved.
        key: String,
    },
}

/// Register and resolve durable waits over the canonical protocol state.
pub struct WaitRegistry;

impl WaitRegistry {
    /// Append a wait for a run, creating empty protocol state when none exists yet.
    ///
    /// # Errors
    /// Returns [`WaitError::Duplicate`] when the run already waits on the same kind and
    /// key, and a store error when the write fails.
    pub async fn register(
        conn: &mut PgConnection,
        tenant_id: &str,
        run_id: &str,
        wait: Wait,
    ) -> Result<(), WaitError> {
        register_in(conn, tenant_id, run_id, wait).await
    }

    /// Remove the matching wait and return it.
    ///
    /// # Errors
    /// Returns [`WaitError::NotRegistered`] when this run holds no wait with the given
    /// kind and key. The run is left untouched in that case, so a mismatched resolution
    /// can never resume work.
    pub async fn resolve(
        conn: &mut PgConnection,
        tenant_id: &str,
        run_id: &str,
        kind: WaitKind,
        key: &str,
    ) -> Result<Wait, WaitError> {
        resolve_in(conn, tenant_id, run_id, kind, key).await
    }

    /// List the waits currently registered for a run.
    ///
    /// # Errors
    /// Returns a store error when the state cannot be read or decoded.
    pub async fn pending(
        conn: &mut PgConnection,
        tenant_id: &str,
        run_id: &str,
    ) -> Result<Vec<Wait>, WaitError> {
        let state = ProtocolStateStore::load(conn, tenant_id, run_id).await?;
        Ok(state.map_or_else(Vec::new, |state| state.waits))
    }
}

/// Append a wait using a caller-owned connection.
pub(crate) async fn register_in(
    conn: &mut PgConnection,
    tenant_id: &str,
    run_id: &str,
    wait: Wait,
) -> Result<(), WaitError> {
    let mut state = load_or_new(conn, tenant_id, run_id).await?;
    if state
        .waits
        .iter()
        .any(|existing| existing.kind == wait.kind && existing.key == wait.key)
    {
        return Err(WaitError::Duplicate {
            run_id: run_id.to_string(),
            kind: wait.kind,
            key: wait.key,
        });
    }
    state.waits.push(wait);
    ProtocolStateStore::store(conn, tenant_id, &state).await?;
    Ok(())
}

/// Remove exactly the matching wait using a caller-owned connection.
pub(crate) async fn resolve_in(
    conn: &mut PgConnection,
    tenant_id: &str,
    run_id: &str,
    kind: WaitKind,
    key: &str,
) -> Result<Wait, WaitError> {
    let not_registered = || WaitError::NotRegistered {
        run_id: run_id.to_string(),
        kind: kind.clone(),
        key: key.to_string(),
    };
    let mut state = ProtocolStateStore::load(conn, tenant_id, run_id)
        .await?
        .ok_or_else(not_registered)?;
    let index = state
        .waits
        .iter()
        .position(|wait| wait.kind == kind && wait.key == key)
        .ok_or_else(not_registered)?;
    let resolved = state.waits.remove(index);
    ProtocolStateStore::store(conn, tenant_id, &state).await?;
    Ok(resolved)
}

/// Load a run's protocol state, or an empty state when the run has none yet.
pub(crate) async fn load_or_new(
    conn: &mut PgConnection,
    tenant_id: &str,
    run_id: &str,
) -> Result<ProtocolState, WaitError> {
    Ok(
        match ProtocolStateStore::load(conn, tenant_id, run_id).await? {
            Some(state) => state,
            None => ProtocolState::new(run_id, 1),
        },
    )
}
