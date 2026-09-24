//! The durable ContextProjection layer (DOSSIER.md §8 layer 3, DOMAIN.md §11.2).
//!
//! What a model call was shown is recorded before the call is made, keyed by the plane's `ctx_`
//! id: the segments with their sources and trust levels, the token ledger and what was dropped.
//! The content itself is not copied — it is canonical state the runtime already holds — so the
//! record says *what* the model saw and *why*, and the per-turn inspector reads it.

use serde_json::{json, Value};
use sqlx::PgPool;

use super::port::ValidatedProjection;
use crate::control::schema;
use crate::runtime::state_machine::RuntimeError;

/// Record a validated projection for `run_id`/`turn_id`; recording the same id again is a no-op.
///
/// # Errors
/// Returns a database error; the caller fails the model call rather than make it unrecorded.
pub async fn record_projection(
    pool: &PgPool,
    tenant_id: &str,
    run_id: &str,
    turn_id: &str,
    projection: &ValidatedProjection,
) -> Result<(), RuntimeError> {
    let segments: Vec<Value> = projection
        .segments
        .iter()
        .map(|(segment, trust)| {
            // `ReceivedProjection::from` spells a segment's source `<kind>:<ref>`.
            let (kind, reference) = segment
                .source
                .split_once(':')
                .unwrap_or(("", segment.source.as_str()));
            json!({
                "segment_id": segment.segment_id,
                "kind": kind,
                "source": reference,
                "trust_level": trust.as_str(),
                "tokens": segment.tokens,
            })
        })
        .collect();
    let degradation: Vec<Value> = projection
        .degradation
        .iter()
        .map(|entry| serde_json::from_str(entry).unwrap_or_else(|_| Value::String(entry.clone())))
        .collect();
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id).await?;
    sqlx::query(
        "INSERT INTO context_projections \
         (id, tenant_id, run_id, turn_id, segments, token_ledger, degradation, policy_snapshot_digest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (id) DO NOTHING",
    )
    .bind(&projection.projection_id)
    .bind(tenant_id)
    .bind(run_id)
    .bind(turn_id)
    .bind(Value::Array(segments))
    .bind(&projection.token_ledger)
    .bind(Value::Array(degradation))
    .bind((!projection.policy_snapshot_digest.is_empty()).then_some(&projection.policy_snapshot_digest))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
