//! The durable ModelCallRecord (DOMAIN.md §11.1, §5.4): the `ModelRoute` and `ModelCall`
//! rows a model-gateway boundary writes at the end of a call, so the per-turn inspector can
//! say which provider and model answered a `model_call` step.
//!
//! Only a model boundary writes here: the in-process conformance seam, and the gateway model
//! source (`intelligence::model_source`), which records the gateway's own `mr_` route and the
//! `ctx_` projection the call was rendered from. The runtime engine never does; it records the
//! step and leaves the call to the boundary that made it.

use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use serde_json::Value;
use sqlx::PgPool;

use crate::control::schema;
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
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &request.tenant_id).await?;
        let mut generator = UlidGenerator::new();
        let route_id = call.route_id.clone().unwrap_or_else(|| {
            CanonicalId::generate(Prefix::ModelRoute, &mut generator).to_string()
        });
        let call_row_id = format!("mcl_{}", generator.generate().to_base32());
        let call_id = format!("call_{}", request.step_id);
        sqlx::query(
            "INSERT INTO model_routes (id, tenant_id, request_class, provider, model_id, config, \
                                       fallbacks, cost_class, chosen_by) \
             VALUES ($1, $2, $3, $4, $5, '{}'::jsonb, '[]'::jsonb, 'free', $6) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&route_id)
        .bind(&request.tenant_id)
        .bind(&call.request_class)
        .bind(&call.provider)
        .bind(&call.model_id)
        .bind(&call.chosen_by)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO model_calls (id, tenant_id, run_id, turn_id, step_id, route_id, call_id, \
                                      status, stop_reason, usage, latency_ms, context_projection_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'completed', $8, $9, $10, $11) \
             ON CONFLICT (tenant_id, call_id) DO NOTHING",
        )
        .bind(&call_row_id)
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
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(call_row_id)
    }
}
