//! The durable budget authority (RUN-010): grants, consumption, exhaustion and capacity.
//!
//! Every decision is written in one event-emitting transaction over the `budgets` row, and a
//! refusal writes nothing. The service touches the budget row and the `usage.*` event stream and
//! nothing else — in particular it never reaches into `effect_records`, so exhausting a budget
//! stops *new* work while an effect already reserved or dispatched is left for the Effect Ledger
//! to settle or reconcile (acceptance 1).

use async_trait::async_trait;
use quansio_events::event_type::EventType;
use quansio_events::{EventDraft, EventStore};
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::limits::{
    applied, ensure_child_within_parent, exhausted_meters, first_exhausted, BudgetConsumed,
    BudgetLimits, MeterUsage,
};
use super::BudgetError;
use crate::control::schema;

/// The aggregate type every usage event is stamped with.
pub const BUDGET_AGGREGATE: &str = "budget";

/// A stored budget (DOMAIN.md §13.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetRecord {
    /// `bdg_…` identity.
    pub id: String,
    /// Owning workspace, for workspace-scoped budgets.
    pub workspace_id: Option<String>,
    /// Scope the budget governs.
    pub scope: String,
    /// The run or agent thread the scope points at.
    pub scope_ref: Option<String>,
    /// Limits in force.
    pub limits: BudgetLimits,
    /// Consumption so far.
    pub consumed: BudgetConsumed,
    /// Parent budget, when the scope is nested.
    pub parent_budget_id: Option<String>,
    /// `active`, `exhausted` or `suspended`.
    pub status: String,
}

impl BudgetRecord {
    /// Whether the budget can be charged at all.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    /// The meters at or beyond their limit.
    #[must_use]
    pub fn exhausted_meters(&self) -> Vec<super::Meter> {
        exhausted_meters(&self.limits, &self.consumed)
    }

    /// The `concurrency` limit this budget sets, when it sets one.
    #[must_use]
    pub fn concurrency_limit(&self) -> Option<usize> {
        self.limits
            .has(super::Meter::Concurrency)
            .then(|| usize::try_from(self.limits.get(super::Meter::Concurrency)).ok())
            .flatten()
    }
}

/// A budget to grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetSpec {
    /// Owning workspace, when the scope is a workspace or below.
    pub workspace_id: Option<String>,
    /// `tenant`, `workspace`, `run` or `agent_thread`.
    pub scope: String,
    /// The run or agent thread the scope points at.
    pub scope_ref: Option<String>,
    /// Limits to grant.
    pub limits: BudgetLimits,
    /// Parent budget, when the scope is nested.
    pub parent_budget_id: Option<String>,
}

/// What a charge did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChargeOutcome {
    /// Budget that was charged.
    pub budget_id: String,
    /// Consumption after the charge.
    pub consumed: BudgetConsumed,
    /// Status after the charge.
    pub status: String,
    /// Whether the charge exhausted the budget.
    pub exhausted: bool,
}

impl ChargeOutcome {
    /// Whether the budget can still fund new work.
    #[must_use]
    pub fn may_continue(&self) -> bool {
        !self.exhausted && self.status == "active"
    }
}

/// The durable budget authority for one tenant.
#[derive(Clone)]
pub struct BudgetService {
    pool: PgPool,
    identity: crate::runtime::state_machine::RuntimeIdentity,
    events: EventStore,
}

impl BudgetService {
    /// Build the service for one tenant.
    ///
    /// # Errors
    /// Returns [`BudgetError`] when the tenant id is not canonical.
    pub fn new(
        pool: PgPool,
        identity: crate::runtime::state_machine::RuntimeIdentity,
    ) -> Result<Self, BudgetError> {
        schema::validate_tenant_id(&identity.tenant_id).map_err(|error| {
            BudgetError::Malformed {
                id: identity.tenant_id.clone(),
                detail: error.to_string(),
            }
        })?;
        Ok(Self {
            events: EventStore::new(pool.clone()),
            pool,
            identity,
        })
    }

    /// The event identity this service stamps on usage events.
    #[must_use]
    pub fn identity(&self) -> &crate::runtime::state_machine::RuntimeIdentity {
        &self.identity
    }

    /// Grant a budget under a caller-supplied id, enforcing `child ≤ parent remaining`
    /// (acceptance 2).
    ///
    /// The id is supplied rather than minted here: the canonical prefix catalog
    /// (`quansio.v1.core.EntityPrefix`, DOMAIN.md §1.1) has no budget prefix, so minting belongs to
    /// whoever completes that catalog, and this module must not invent one.
    ///
    /// # Errors
    /// Returns [`BudgetError::ExceedsParent`] when a meter asks for more than the parent has left,
    /// and [`BudgetError::NotFound`] when the named parent does not exist.
    pub async fn create(
        &self,
        budget_id: &str,
        spec: BudgetSpec,
    ) -> Result<BudgetRecord, BudgetError> {
        if let Some(parent_id) = &spec.parent_budget_id {
            let parent = self.load(parent_id).await?;
            ensure_child_within_parent(
                "(new budget)",
                parent_id,
                &spec.limits,
                &parent.limits,
                &parent.consumed,
            )?;
        }
        let id = budget_id.to_string();
        let limits = spec.limits.to_json();
        let consumed = spec.consumed_json();
        let mut tx = self.tenant_tx().await?;
        sqlx::query(
            "INSERT INTO budgets (id, tenant_id, workspace_id, scope, scope_ref, limits, consumed, \
             parent_budget_id, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active')",
        )
        .bind(&id)
        .bind(&self.identity.tenant_id)
        .bind(&spec.workspace_id)
        .bind(&spec.scope)
        .bind(&spec.scope_ref)
        .bind(&limits)
        .bind(&consumed)
        .bind(&spec.parent_budget_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.load(&id).await
    }

    /// Load a budget.
    ///
    /// # Errors
    /// Returns [`BudgetError::NotFound`] when the budget is not visible to this tenant.
    pub async fn load(&self, budget_id: &str) -> Result<BudgetRecord, BudgetError> {
        let mut tx = self.tenant_tx().await?;
        let row = sqlx::query(
            "SELECT id, workspace_id, scope, scope_ref, limits, consumed, parent_budget_id, status \
             FROM budgets WHERE id = $1 AND tenant_id = $2",
        )
        .bind(budget_id)
        .bind(&self.identity.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some(row) = row else {
            return Err(BudgetError::NotFound {
                id: budget_id.to_string(),
            });
        };
        decode(&row)
    }

    /// The budget governing a scope, when one was granted.
    ///
    /// # Errors
    /// Returns a database error when the lookup fails.
    pub async fn for_scope(
        &self,
        scope: &str,
        scope_ref: Option<&str>,
    ) -> Result<Option<BudgetRecord>, BudgetError> {
        let mut tx = self.tenant_tx().await?;
        let row = sqlx::query(
            "SELECT id, workspace_id, scope, scope_ref, limits, consumed, parent_budget_id, status \
             FROM budgets WHERE tenant_id = $1 AND scope = $2 \
             AND ($3::text IS NULL OR scope_ref = $3) ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&self.identity.tenant_id)
        .bind(scope)
        .bind(scope_ref)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(decode).transpose()
    }

    /// Charge a budget, emitting a `usage.*` event.
    ///
    /// A charge that would exceed a limit is refused with [`BudgetError::Exhausted`]; the budget is
    /// marked `exhausted` and a `usage.exhausted` event records why, so the caller stops new work
    /// while anything already in flight is untouched (acceptance 1).
    ///
    /// # Errors
    /// Returns [`BudgetError::Exhausted`] for a charge beyond a limit and
    /// [`BudgetError::NotFound`] when the budget is unknown.
    pub async fn charge(
        &self,
        budget_id: &str,
        charges: &MeterUsage,
    ) -> Result<ChargeOutcome, BudgetError> {
        let record = self.load(budget_id).await?;
        if let Some((meter, consumed, limit)) =
            first_exhausted(&record.limits, &record.consumed, charges)
        {
            self.mark_exhausted(budget_id, meter, consumed, limit)
                .await?;
            return Err(BudgetError::Exhausted {
                budget_id: budget_id.to_string(),
                meter,
                consumed,
                limit,
            });
        }
        let next = applied(&record.consumed, charges);
        let meters = exhausted_meters(&record.limits, &next);
        let status = if meters.is_empty() {
            "active"
        } else {
            "exhausted"
        };
        self.store_consumed(budget_id, &next, status).await?;
        self.emit(
            budget_id,
            "usage.recorded",
            json!({
                "budget_id": budget_id,
                "scope": record.scope,
                "scope_ref": record.scope_ref,
                "charged": charges.to_json(),
                "consumed": next.to_json(),
                "status": status,
            }),
        )
        .await?;
        Ok(ChargeOutcome {
            budget_id: budget_id.to_string(),
            consumed: next,
            status: status.to_string(),
            exhausted: !meters.is_empty(),
        })
    }

    /// The concurrency limit a budget sets, or `0` once it is exhausted.
    ///
    /// This is the number RUN-004's orchestration capacity gate takes from its caller, so a
    /// exhausted budget stops new runs being admitted at all.
    ///
    /// # Errors
    /// Returns [`BudgetError::NotFound`] when the budget is unknown.
    pub async fn capacity_limit(&self, budget_id: &str) -> Result<Option<usize>, BudgetError> {
        let record = self.load(budget_id).await?;
        if !record.is_active() {
            return Ok(Some(0));
        }
        Ok(record.concurrency_limit())
    }

    /// What a parent has left, for granting a child budget.
    ///
    /// # Errors
    /// Returns [`BudgetError::NotFound`] when the parent is unknown.
    pub async fn parent_remaining(
        &self,
        parent_budget_id: &str,
    ) -> Result<BudgetLimits, BudgetError> {
        let parent = self.load(parent_budget_id).await?;
        let mut remaining = BudgetLimits::new();
        for (meter, limit) in parent.limits.entries() {
            remaining.set(*meter, limit.saturating_sub(parent.consumed.get(*meter)));
        }
        Ok(remaining)
    }

    async fn tenant_tx(&self) -> Result<Transaction<'static, Postgres>, BudgetError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.identity.tenant_id)
            .await
            .map_err(|error| BudgetError::Database(sqlx::Error::Protocol(error.to_string())))?;
        Ok(tx)
    }

    async fn mark_exhausted(
        &self,
        budget_id: &str,
        meter: super::Meter,
        consumed: u64,
        limit: u64,
    ) -> Result<(), BudgetError> {
        let record = self.load(budget_id).await?;
        self.store_consumed(budget_id, &record.consumed, "exhausted")
            .await?;
        self.emit(
            budget_id,
            "usage.exhausted",
            json!({
                "budget_id": budget_id,
                "scope": record.scope,
                "scope_ref": record.scope_ref,
                "meter": meter.as_str(),
                "consumed": consumed,
                "limit": limit,
            }),
        )
        .await
    }

    async fn store_consumed(
        &self,
        budget_id: &str,
        consumed: &BudgetConsumed,
        status: &str,
    ) -> Result<(), BudgetError> {
        let mut tx = self.tenant_tx().await?;
        sqlx::query(
            "UPDATE budgets SET consumed = $3, status = $4, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(budget_id)
        .bind(&self.identity.tenant_id)
        .bind(consumed.to_json())
        .bind(status)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn emit(&self, budget_id: &str, event: &str, payload: Value) -> Result<(), BudgetError> {
        let mut tx = self.tenant_tx().await?;
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(aggregate_version), 0) + 1 FROM runtime_events \
             WHERE tenant_id = $1 AND aggregate_type = $2 AND aggregate_id = $3",
        )
        .bind(&self.identity.tenant_id)
        .bind(BUDGET_AGGREGATE)
        .bind(budget_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let draft = EventDraft::new(
            BUDGET_AGGREGATE,
            budget_id,
            u64::try_from(next).unwrap_or(1),
            EventType::parse(event)?,
            self.identity.correlation_id,
            self.identity.actor.clone(),
        )
        .with_payload(payload);
        let tenant_id = self.identity.tenant_id.clone();
        self.events
            .commit_mutation_tx(&tenant_id, move |_tx, batch| {
                Box::pin(async move {
                    batch.emit(draft);
                    Ok(())
                })
            })
            .await?;
        Ok(())
    }
}

impl BudgetSpec {
    fn consumed_json(&self) -> Value {
        BudgetConsumed::new().to_json()
    }
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<BudgetRecord, BudgetError> {
    let id: String = row.try_get("id")?;
    let limits: Value = row.try_get("limits")?;
    let consumed: Value = row.try_get("consumed")?;
    Ok(BudgetRecord {
        limits: BudgetLimits::parse(&id, &limits)?,
        consumed: BudgetConsumed::parse(&id, &consumed)?,
        id,
        workspace_id: row.try_get("workspace_id")?,
        scope: row.try_get("scope")?,
        scope_ref: row.try_get("scope_ref")?,
        parent_budget_id: row.try_get("parent_budget_id")?,
        status: row.try_get("status")?,
    })
}

/// The budget port, so a composition root can bound work without the concrete store.
#[async_trait]
pub trait BudgetPort: Send + Sync {
    /// Charge a budget.
    ///
    /// # Errors
    /// Returns the refusal when the charge cannot fit.
    async fn charge(
        &self,
        budget_id: &str,
        charges: &MeterUsage,
    ) -> Result<ChargeOutcome, BudgetError>;
}

#[async_trait]
impl BudgetPort for BudgetService {
    async fn charge(
        &self,
        budget_id: &str,
        charges: &MeterUsage,
    ) -> Result<ChargeOutcome, BudgetError> {
        BudgetService::charge(self, budget_id, charges).await
    }
}
