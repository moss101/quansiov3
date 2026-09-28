//! The durable ModelCallRecord (DOMAIN.md §11.1, §5.4): the `ModelRoute` and `ModelCall`
//! rows a model-gateway boundary writes at the end of a call, so the per-turn inspector can
//! say which provider and model answered a `model_call` step.
//!
//! Only a model boundary writes here: the in-process conformance seam, and the gateway model
//! source (`intelligence::model_source`), which records the gateway's own `mr_` route and the
//! `ctx_` projection the call was rendered from. The runtime engine never does; it records the
//! step and leaves the call to the boundary that made it.
//!
//! The call row and its `model.usage_recorded` RuntimeEvent commit together, so OPS-004's usage
//! projection meters exactly the calls the ledger holds; a repeated record of the same call is a
//! no-op for both.

use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::runtime::state_machine::{ModelCallRequest, RuntimeError};

/// What the boundary knows about a finished call.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    /// Provider that answered (a `config/models.yaml` provider, or the seam's own name).
    pub provider: String,
    /// Model that answered.
    pub model_id: String,
    /// Request class the route was chosen for.
    pub request_class: String,
    /// Rule id that chose the route.
    pub chosen_by: String,
    /// Normalized stop reason.
    pub stop_reason: String,
    /// Wall-clock latency of the call.
    pub latency_ms: i64,
    /// Usage as the provider reported it.
    pub usage: Value,
    /// The `mr_` id the gateway minted for the route, when a gateway answered; the ledger
    /// mints one otherwise.
    pub route_id: Option<String>,
    /// The `ctx_` ContextProjection the call was rendered from, when there was one.
    pub context_projection_id: Option<String>,
}

/// The ledger of model calls.
#[derive(Debug, Clone)]
pub struct ModelCallLedger {
    pool: PgPool,
}

impl ModelCallLedger {
    /// A ledger over the pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record the route and the call for `request`; returns the `mcl_` row id. A second
    /// record of the same step's call is a no-op (the call id is derived from the step).
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn record(
        &self,
        request: &ModelCallRequest,
        call: &RecordedCall,
    ) -> Result<String, RuntimeError> {
        let mut generator = UlidGenerator::new();
        let route_id = call.route_id.clone().unwrap_or_else(|| {
            CanonicalId::generate(Prefix::ModelRoute, &mut generator).to_string()
        });
        let call_row_id = format!("mcl_{}", generator.generate().to_base32());
        let correlation = CorrelationId::generate(&mut generator);
        let request = request.clone();
        let call = call.clone();
        let row_id = call_row_id.clone();
        let result = EventStore::new(self.pool.clone())
            .commit_mutation(&request.tenant_id.clone(), move |conn, batch| {
                Box::pin(async move {
                    let call_id = format!("call_{}", request.step_id);
                    sqlx::query(
                        "INSERT INTO model_routes (id, tenant_id, request_class, provider, model_id, \
                                                   config, fallbacks, cost_class, chosen_by) \
                         VALUES ($1, $2, $3, $4, $5, '{}'::jsonb, '[]'::jsonb, 'free', $6) \
                         ON CONFLICT (id) DO NOTHING",
                    )
                    .bind(&route_id)
                    .bind(&request.tenant_id)
                    .bind(&call.request_class)
                    .bind(&call.provider)
                    .bind(&call.model_id)
                    .bind(&call.chosen_by)
                    .execute(&mut *conn)
                    .await?;
                    let inserted = sqlx::query(
                        "INSERT INTO model_calls (id, tenant_id, run_id, turn_id, step_id, route_id, \
                                                  call_id, status, stop_reason, usage, latency_ms, \
                                                  context_projection_id) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, 'completed', $8, $9, $10, $11) \
                         ON CONFLICT (tenant_id, call_id) DO NOTHING",
                    )
                    .bind(&row_id)
                    .bind(&request.tenant_id)
                    .bind(&request.run_id)
                    .bind(&request.turn_id)
                    .bind(&request.step_id)
                    .bind(&route_id)
                    .bind(&call_id)
                    .bind(&call.stop_reason)
                    .bind(&call.usage)
                    .bind(call.latency_ms)
                    .bind(call.context_projection_id.as_deref())
                    .execute(&mut *conn)
                    .await?
                    .rows_affected();
                    if inserted == 0 {
                        // Already recorded: nothing new to meter.
                        return Ok(());
                    }
                    let workspace_id: Option<String> =
                        sqlx::query_scalar("SELECT workspace_id FROM runs WHERE id = $1 AND tenant_id = $2")
                            .bind(&request.run_id)
                            .bind(&request.tenant_id)
                            .fetch_optional(&mut *conn)
                            .await?;
                    let mut draft = EventDraft::new(
                        "model_call",
                        row_id.clone(),
                        1,
                        EventType::parse("model.usage_recorded")?,
                        correlation,
                        Actor::system("model-gateway"),
                    )
                    .with_payload(usage_payload(&request, &call, &call_id));
                    if let Some(workspace_id) = workspace_id {
                        draft = draft.with_workspace(workspace_id);
                    }
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await;
        match result {
            // Nothing staged: the call was already recorded, and the transaction rolled back.
            Ok(()) | Err(EventError::NoEventStaged) => Ok(call_row_id),
            Err(error) => Err(error.into()),
        }
    }

    /// The model that answered `run_id`'s latest recorded call — the model that made a claim
    /// the run is verifying (RUN-008) — or `None` when the run recorded no call.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn claimant_model(
        &self,
        tenant_id: &str,
        run_id: &str,
    ) -> Result<Option<String>, RuntimeError> {
        let mut tx = self.pool.begin().await?;
        crate::control::schema::set_tenant_context(&mut tx, tenant_id).await?;
        let model: Option<String> = sqlx::query_scalar(
            "SELECT r.model_id FROM model_calls c JOIN model_routes r ON r.id = c.route_id \
             WHERE c.tenant_id = $1 AND c.run_id = $2 \
             ORDER BY c.created_at DESC, c.id DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(model.filter(|model| !model.is_empty()))
    }
}

/// The metered quantities of one call: the provider's reported tokens under the names the usage
/// projection meters (`input_tokens`, `output_tokens`, `cache_tokens`).
fn usage_payload(request: &ModelCallRequest, call: &RecordedCall, call_id: &str) -> Value {
    let tokens = |field: &str| call.usage.get(field).and_then(Value::as_i64).unwrap_or(0);
    json!({
        "call_id": call_id,
        "run_id": request.run_id,
        "turn_id": request.turn_id,
        "step_id": request.step_id,
        "provider": call.provider,
        "model_id": call.model_id,
        "input_tokens": tokens("input"),
        "output_tokens": tokens("output"),
        "cache_tokens": tokens("cache_read") + tokens("cache_write"),
    })
}
