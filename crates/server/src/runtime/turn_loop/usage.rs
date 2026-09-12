//! Per-turn usage: which model call, context projection and tools a turn used
//! (DOMAIN.md §5.3, §5.6).
//!
//! This is a read model over the rows the runtime already writes — `turns`,
//! `steps` and `tool_calls` — so inspection never needs a second source of truth. The
//! model route of a model call belongs to INT-002's ModelCallRecord and is not duplicated
//! here.

use quansio_core::{CanonicalId, Prefix};
use sqlx::{PgPool, Row};

use crate::control::schema;

use super::super::state_machine::{RuntimeError, RuntimeIdentity};

/// One tool call a turn made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUsageEntry {
    /// `tc_…` tool call row.
    pub tool_call_id: String,
    /// Registered tool name.
    pub tool: String,
    /// Lifecycle status the call reached.
    pub status: String,
    /// Effect the call reserved, when it got that far.
    pub effect_id: Option<String>,
    /// Trust level assigned to the result.
    pub trust_level: i16,
    /// Tool declaration version the call was planned against.
    pub declaration_version: u32,
}

/// What a turn used, as the UI inspects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnUsage {
    /// Run the turn belongs to.
    pub run_id: String,
    /// Turn identity.
    pub turn_id: String,
    /// Context projection the turn was built from, when one was supplied.
    pub context_projection_id: Option<String>,
    /// Steps the turn recorded.
    pub step_count: i32,
    /// Tool calls the turn made, in step order.
    pub tools: Vec<ToolUsageEntry>,
}

/// Load the usage record for one turn.
///
/// # Errors
/// Returns [`RuntimeError::NotFound`] when the turn is not visible to this tenant.
pub async fn load_turn_usage(
    pool: &PgPool,
    identity: &RuntimeIdentity,
    turn_id: &str,
) -> Result<TurnUsage, RuntimeError> {
    schema::validate_tenant_id(&identity.tenant_id)?;
    let turn = CanonicalId::parse_typed(turn_id, Prefix::Turn)?;
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, &identity.tenant_id).await?;
    let turn_row = sqlx::query(
        "SELECT run_id, context_projection_id, step_count FROM turns \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(turn.to_string())
    .bind(&identity.tenant_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| RuntimeError::NotFound {
        entity: "turn",
        id: turn_id.to_string(),
        tenant_id: identity.tenant_id.clone(),
    })?;

    let rows = sqlx::query(
        "SELECT tc.id, tc.tool_name, tc.status, tc.effect_id, tc.trust_level, \
         tc.declaration_version \
         FROM tool_calls tc JOIN steps s ON s.id = tc.step_id \
         WHERE s.turn_id = $1 AND tc.tenant_id = $2 ORDER BY s.seq, tc.created_at",
    )
    .bind(turn.to_string())
    .bind(&identity.tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut tools = Vec::with_capacity(rows.len());
    for row in &rows {
        tools.push(ToolUsageEntry {
            tool_call_id: row.try_get("id")?,
            tool: row.try_get("tool_name")?,
            status: row.try_get("status")?,
            effect_id: row.try_get("effect_id")?,
            trust_level: row.try_get("trust_level")?,
            declaration_version: u32::try_from(row.try_get::<i32, _>("declaration_version")?)
                .unwrap_or(1),
        });
    }
    Ok(TurnUsage {
        run_id: turn_row.try_get("run_id")?,
        turn_id: turn.to_string(),
        context_projection_id: turn_row.try_get("context_projection_id")?,
        step_count: turn_row.try_get("step_count")?,
        tools,
    })
}
