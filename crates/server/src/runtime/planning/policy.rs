//! The effective `policies.max_plan_nodes` bound (DOMAIN.md §4.5).
//!
//! A workspace policy may only narrow the tenant policy, so the bound is the smallest
//! `max_plan_nodes` across the tenant row and the workspace row. With no policy row the
//! conservative [`DEFAULT_MAX_PLAN_NODES`] applies. Policy rows are owned by the server's
//! control/policy surface, so the planning module reads them directly; the WorkGraph is
//! never read here.

use sqlx::PgPool;

use super::compile::DEFAULT_MAX_PLAN_NODES;
use super::PlanError;

/// The effective node bound for `workspace_id` in this tenant.
///
/// # Errors
/// Returns [`PlanError::Workspace`] when the policy read fails.
pub async fn max_plan_nodes(
    pool: &PgPool,
    tenant_id: &str,
    workspace_id: &str,
) -> Result<usize, PlanError> {
    let mut tx = pool.begin().await.map_err(workspace_error)?;
    crate::control::schema::set_tenant_context(&mut tx, tenant_id)
        .await
        .map_err(workspace_error)?;
    let limit: Option<i32> = sqlx::query_scalar(
        "SELECT MIN(max_plan_nodes) FROM policies WHERE tenant_id = $1 \
         AND ((scope = 'tenant' AND workspace_id IS NULL) OR workspace_id = $2)",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(workspace_error)?;
    tx.commit().await.map_err(workspace_error)?;
    Ok(limit
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_MAX_PLAN_NODES))
}

/// Wrap a database/policy failure as a plan workspace error.
fn workspace_error(error: impl std::fmt::Display) -> PlanError {
    PlanError::Workspace {
        detail: format!("policy bound: {error}"),
    }
}
