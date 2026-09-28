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

/// The segments the model was shown during one turn, with the trust label each was recorded
/// under (`None` when the record carries no recognisable label), across every projection the
/// turn's model calls were given, oldest first.
///
/// This is the proposal's causal window (DOMAIN.md §12 rule 2): a tool call a turn proposes
/// derives from what that turn's model calls saw. The labels are returned as recorded; the
/// policy module decides what an absent or unknown label means.
///
/// # Errors
/// Returns [`RuntimeError::Database`] when the projections cannot be read.
pub async fn turn_segment_labels(
    pool: &PgPool,
    tenant_id: &str,
    run_id: &str,
    turn_id: &str,
) -> Result<Vec<(String, Option<String>)>, RuntimeError> {
    let mut tx = pool.begin().await?;
    schema::set_tenant_context(&mut tx, tenant_id).await?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT segments FROM context_projections \
         WHERE tenant_id = $1 AND run_id = $2 AND turn_id = $3 ORDER BY created_at, id",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(turn_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut labels = Vec::new();
    for segments in rows {
        let Some(segments) = segments.as_array() else {
            // A record whose segments are not a list proves nothing about what was shown.
            labels.push((String::new(), None));
            continue;
        };
        for (index, segment) in segments.iter().enumerate() {
            let id = segment
                .get("segment_id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("#{index}"), str::to_string);
            let level = segment
                .get("trust_level")
                .and_then(Value::as_str)
                .map(str::to_string);
            labels.push((id, level));
        }
    }
    Ok(labels)
}
