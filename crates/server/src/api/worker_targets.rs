//! `ProvisionTarget` for a worker that dials in (EXEC-002, DOMAIN.md §8.3.2).
//!
//! A `customer_private_worker` is the execution target a person's own computer provides: the worker daemon dials the
//! server's worker socket and the hub takes the lease. This command is how the target gets registered, so a person can
//! start a worker from the app instead of seeding the database. It serves that one substrate; every other substrate is a
//! provisioning flow of its own and is refused here, never answered by a stand-in.

use serde_json::{json, Value};
use sqlx::Row;

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_machine::control::{
    MachineControl, NewTarget, Substrate, TargetClass, TargetFence, TargetStatus,
};

use super::live_sessions::{actor_of, audit};
use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::policy::rbac::ActionFamily;
use crate::runtime::state_machine::RuntimeIdentity;

/// `ProvisionTarget {workspace_id, substrate: "customer_private_worker", target_class?}`.
///
/// One live worker target per workspace: asking again returns the one that exists (`created: false`), whatever its state
/// — a worker that is not attached is started again against the same target, not given a second. The answer names what a
/// worker needs to dial in: the target, the tenant, and the socket the server's hub listens on (absent when the server
/// has no worker socket, which means no worker can attach).
///
/// # Errors
/// `VALIDATION_SCHEMA` for a missing field or a substrate this command does not serve, `SCOPE_FORBIDDEN` unless the caller
/// may manage the workspace.
pub async fn provision_target(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    user_id: Option<&str>,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let invalid =
        |detail: String| ApiError::new(ApiErrorCode::ValidationSchema, detail, correlation);
    let workspace_id = params
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("ws_"))
        .ok_or_else(|| invalid("workspace_id is required".to_string()))?;
    let substrate = params
        .get("substrate")
        .and_then(Value::as_str)
        .unwrap_or("");
    if substrate != Substrate::CustomerPrivateWorker.as_str() {
        return Err(invalid(format!(
            "ProvisionTarget serves the customer_private_worker substrate here; {substrate:?} is provisioned by its own flow"
        )));
    }
    let class = match params.get("target_class").and_then(Value::as_str) {
        None | Some("persistent_workspace_computer") => TargetClass::PersistentWorkspaceComputer,
        Some(other) => {
            return Err(invalid(format!(
                "target_class {other:?}: a worker is a persistent_workspace_computer"
            )))
        }
    };
    Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::ManageWorkspace,
        correlation,
    )
    .await?;

    let mut tx = state.tenant_tx(&scope.tenant_id, correlation).await?;
    let database = |error: &dyn std::fmt::Display| {
        ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation)
    };
    // Two asks at once register one target: the workspace's row is the lock.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("worker_target:{}:{workspace_id}", scope.tenant_id))
        .execute(&mut *tx)
        .await
        .map_err(|error| database(&error))?;
    let existing = sqlx::query(
        "SELECT id, status FROM execution_targets WHERE tenant_id = $1 AND workspace_id = $2 AND substrate = $3 \
         AND status NOT IN ('DESTROYED', 'FAILED') ORDER BY created_at, id LIMIT 1",
    )
    .bind(&scope.tenant_id)
    .bind(workspace_id)
    .bind(Substrate::CustomerPrivateWorker.as_str())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database(&error))?;
    let (target_id, status, created) = if let Some(row) = existing {
        (
            row.get::<String, _>("id"),
            row.get::<String, _>("status"),
            false,
        )
    } else {
        let target_id =
            CanonicalId::generate(Prefix::ExecutionTarget, &mut UlidGenerator::new()).to_string();
        MachineControl::register(
            &mut tx,
            &scope.tenant_id,
            &NewTarget {
                id: target_id.clone(),
                workspace_id: workspace_id.to_string(),
                class,
                substrate: Substrate::CustomerPrivateWorker,
                image_digest: None,
                desired_state: Some("READY".to_string()),
            },
        )
        .await
        .map_err(|error| database(&error))?;
        // The worker has nothing to build: the target is ready for the worker to lease as soon as it exists.
        for to in [TargetStatus::Provisioning, TargetStatus::Ready] {
            let current = MachineControl::load(&mut tx, &scope.tenant_id, &target_id)
                .await
                .map_err(|error| database(&error))?;
            MachineControl::transition(
                &mut tx,
                &scope.tenant_id,
                &target_id,
                &TargetFence {
                    generation: current.generation,
                },
                to,
            )
            .await
            .map_err(|error| database(&error))?;
        }
        audit(
            &mut tx,
            &scope.tenant_id,
            workspace_id,
            &actor_of(identity, user_id),
            "ProvisionTarget",
            &format!("target:{target_id}"),
            "registered a customer_private_worker target",
            correlation,
        )
        .await?;
        (target_id, TargetStatus::Ready.as_str().to_string(), true)
    };
    tx.commit().await.map_err(|error| database(&error))?;

    let hub = state.worker_hub();
    Ok(json!({
        "target_id": target_id,
        "workspace_id": workspace_id,
        "tenant_id": scope.tenant_id,
        "substrate": Substrate::CustomerPrivateWorker.as_str(),
        "target_class": class.as_str(),
        "status": status,
        "created": created,
        "worker_socket": hub.and_then(crate::worker::WorkerHub::socket_path).map(|path| path.display().to_string()),
        "attached": hub.is_some_and(|hub| hub.links().iter().any(|link| link.tenant_id == scope.tenant_id && link.target_id == target_id)),
    }))
}
