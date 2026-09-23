//! Routine definitions (`routines`) and their commands.

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

use super::ledger::{self, claim_in, Fire, FireSource};
use crate::scheduler::{AbsencePolicy, RoutineTrigger, RoutineTriggerKind};

/// Repository path used to attribute a rejected mutation.
pub const ROUTINES_OWNER: &str = "crates/server/src/control/routines";

const COLUMNS: &str = "id, workspace_id, owner_user_id, teammate_id, trigger, objective_template, \
                       budget, notification_prefs, absence_policy, status, last_fired_at, \
                       next_due_at, created_at, updated_at";

/// Why a routine command was refused.
#[derive(Debug, thiserror::Error)]
pub enum RoutineError {
    /// The event store or database refused.
    #[error("routine store: {0}")]
    Event(#[from] EventError),
    /// PostgreSQL rejected a statement.
    #[error("routine database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The definition is malformed.
    #[error("routine schema: {0}")]
    Validation(String),
    /// No such routine here.
    #[error("routine {0} was not found")]
    NotFound(String),
    /// The routine's state does not allow it.
    #[error("{0}")]
    Conflict(String),
}

impl RoutineError {
    /// DOMAIN.md §15 code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "VALIDATION_SCHEMA",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT_STATE",
            Self::Event(_) | Self::Database(_) => "INTERNAL",
        }
    }
}

/// A routine as the projection shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineView {
    /// `rtn_…`.
    pub id: String,
    /// Workspace.
    pub workspace_id: String,
    /// The person it runs for (and notifies).
    pub owner_user_id: String,
    /// The teammate that does the work (`agt_…`); the workspace default when absent.
    pub teammate_id: Option<String>,
    /// `{kind, spec, timezone}`.
    pub trigger: Value,
    /// `{title, prompt, completion_contract?}`.
    pub objective_template: Value,
    /// Budget limits.
    pub budget: Value,
    /// Routine-level notification preferences.
    pub notification_prefs: Value,
    /// `skip`, `queue` or `catch_up_once`.
    pub absence_policy: String,
    /// `active`, `paused` or `deleted`.
    pub status: String,
    /// When it last fired.
    pub last_fired_at: Option<DateTime<Utc>>,
    /// When it is next due (schedule triggers).
    pub next_due_at: Option<DateTime<Utc>>,
    /// Created.
    pub created_at: DateTime<Utc>,
}

impl RoutineView {
    /// The projection's spelling.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "workspace_id": self.workspace_id,
            "owner_user_id": self.owner_user_id,
            "teammate_id": self.teammate_id,
            "trigger": self.trigger,
            "objective_template": self.objective_template,
            "budget": self.budget,
            "notification_prefs": self.notification_prefs,
            "absence_policy": self.absence_policy,
            "status": self.status,
            "last_fired_at": self.last_fired_at,
            "next_due_at": self.next_due_at,
            "created_at": self.created_at,
        })
    }

    /// The parsed trigger.
    ///
    /// # Errors
    /// Returns [`RoutineError::Validation`] for a stored trigger that does not parse.
    pub fn trigger(&self) -> Result<RoutineTrigger, RoutineError> {
        serde_json::from_value(self.trigger.clone())
            .map_err(|error| RoutineError::Validation(format!("trigger: {error}")))
    }
}

/// A new routine.
#[derive(Debug, Clone)]
pub struct NewRoutine {
    /// Workspace.
    pub workspace_id: String,
    /// Owner.
    pub owner_user_id: String,
    /// Teammate.
    pub teammate_id: Option<String>,
    /// Trigger.
    pub trigger: RoutineTrigger,
    /// `{title, prompt}`.
    pub objective_template: Value,
    /// Budget.
    pub budget: Value,
    /// Preferences.
    pub notification_prefs: Value,
    /// Absence policy.
    pub absence_policy: AbsencePolicy,
}

/// A change to a routine; `None` leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct RoutineChange {
    /// New trigger.
    pub trigger: Option<RoutineTrigger>,
    /// New template.
    pub objective_template: Option<Value>,
    /// New teammate (`Some(None)` clears it).
    pub teammate_id: Option<Option<String>>,
    /// New budget.
    pub budget: Option<Value>,
    /// New preferences.
    pub notification_prefs: Option<Value>,
    /// New absence policy.
    pub absence_policy: Option<AbsencePolicy>,
}

/// Refuse a template without a title and a prompt of sane size.
///
/// # Errors
/// Returns [`RoutineError::Validation`].
pub fn validate_template(template: &Value) -> Result<(), RoutineError> {
    let title = template
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let prompt = template
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(RoutineError::Validation(
            "objective_template.title is 1..=200 characters".to_string(),
        ));
    }
    if prompt.is_empty() || prompt.len() > 16_384 {
        return Err(RoutineError::Validation(
            "objective_template.prompt is 1..=16384 bytes".to_string(),
        ));
    }
    Ok(())
}

fn validate_trigger(trigger: &RoutineTrigger) -> Result<(), RoutineError> {
    trigger
        .validate()
        .map_err(|error| RoutineError::Validation(error.to_string()))?;
    if trigger.kind == RoutineTriggerKind::Event {
        super::triggers::validate_event_spec(&trigger.spec)?;
    }
    Ok(())
}

/// The routine definitions for one tenant.
#[derive(Debug, Clone)]
pub struct RoutineStore {
    events: EventStore,
    tenant_id: String,
    actor: Actor,
}

impl RoutineStore {
    /// A store acting as `actor` in one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>, actor: Actor) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
            actor,
        }
    }

    /// Every routine of a workspace that is not deleted, newest first.
    ///
    /// # Errors
    /// Returns a database error.
    pub async fn list(&self, workspace_id: &str) -> Result<Vec<RoutineView>, RoutineError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM routines WHERE tenant_id = $1 AND workspace_id = $2 \
             AND status <> 'deleted' ORDER BY created_at DESC, id DESC"
        ))
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(view_from_row).collect()
    }

    /// One routine.
    ///
    /// # Errors
    /// Returns [`RoutineError::NotFound`].
    pub async fn get(&self, routine_id: &str) -> Result<RoutineView, RoutineError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM routines WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(&self.tenant_id)
        .bind(routine_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref()
            .map(view_from_row)
            .transpose()?
            .ok_or_else(|| RoutineError::NotFound(routine_id.to_string()))
    }

    /// Create a routine; a schedule trigger's first window is computed now.
    ///
    /// # Errors
    /// Returns [`RoutineError::Validation`] for a malformed trigger or template.
    pub async fn create(
        &self,
        new: NewRoutine,
        now: DateTime<Utc>,
    ) -> Result<RoutineView, RoutineError> {
        validate_trigger(&new.trigger)?;
        validate_template(&new.objective_template)?;
        let next_due = new
            .trigger
            .initial_due(now)
            .map_err(|error| RoutineError::Validation(error.to_string()))?;
        let id = CanonicalId::generate(Prefix::Routine, &mut UlidGenerator::new()).to_string();
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let trigger = serde_json::to_value(&new.trigger)
            .map_err(|error| RoutineError::Validation(error.to_string()))?;
        self.commit(move |conn, batch| {
            Box::pin(async move {
                let row = sqlx::query(&format!(
                    "INSERT INTO routines (id, tenant_id, workspace_id, owner_user_id, teammate_id, trigger, \
                                           objective_template, budget, notification_prefs, absence_policy, \
                                           status, next_due_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'active', $11) RETURNING {COLUMNS}"
                ))
                .bind(&id)
                .bind(&tenant)
                .bind(&new.workspace_id)
                .bind(&new.owner_user_id)
                .bind(&new.teammate_id)
                .bind(&trigger)
                .bind(&new.objective_template)
                .bind(&new.budget)
                .bind(&new.notification_prefs)
                .bind(new.absence_policy.as_db_str())
                .bind(next_due)
                .fetch_one(&mut *conn)
                .await?;
                let view = view_from_row(&row)?;
                stage(conn, batch, &tenant, &actor, &view, "routine.created", json!({
                    "routine_id": view.id,
                    "trigger": view.trigger,
                    "next_due_at": view.next_due_at,
                }))
                .await?;
                Ok(view)
            })
        })
        .await
    }

    /// Change a routine that is not deleted; a new trigger recomputes the next window.
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] for a deleted routine.
    pub async fn update(
        &self,
        routine_id: &str,
        change: RoutineChange,
        now: DateTime<Utc>,
    ) -> Result<RoutineView, RoutineError> {
        if let Some(trigger) = &change.trigger {
            validate_trigger(trigger)?;
        }
        if let Some(template) = &change.objective_template {
            validate_template(template)?;
        }
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let routine_id = routine_id.to_string();
        self.commit(move |conn, batch| {
            Box::pin(async move {
                let current = lock(conn, &tenant, &routine_id).await?;
                if current.status == "deleted" {
                    return Err(RoutineError::Conflict(format!("routine {routine_id} is deleted")));
                }
                let trigger_changed = change.trigger.is_some();
                let trigger = match &change.trigger {
                    Some(trigger) => serde_json::to_value(trigger)
                        .map_err(|error| RoutineError::Validation(error.to_string()))?,
                    None => current.trigger.clone(),
                };
                let next_due = if trigger_changed && current.status == "active" {
                    change
                        .trigger
                        .as_ref()
                        .map(|trigger| trigger.initial_due(now))
                        .transpose()
                        .map_err(|error| RoutineError::Validation(error.to_string()))?
                        .flatten()
                } else {
                    current.next_due_at
                };
                let teammate = match change.teammate_id {
                    Some(value) => value,
                    None => current.teammate_id.clone(),
                };
                let absence = change
                    .absence_policy
                    .map_or_else(|| current.absence_policy.clone(), |policy| policy.as_db_str().to_string());
                let row = sqlx::query(&format!(
                    "UPDATE routines SET trigger = $3, objective_template = $4, teammate_id = $5, budget = $6, \
                            notification_prefs = $7, absence_policy = $8, next_due_at = $9 \
                     WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
                ))
                .bind(&tenant)
                .bind(&routine_id)
                .bind(&trigger)
                .bind(change.objective_template.as_ref().unwrap_or(&current.objective_template))
                .bind(&teammate)
                .bind(change.budget.as_ref().unwrap_or(&current.budget))
                .bind(change.notification_prefs.as_ref().unwrap_or(&current.notification_prefs))
                .bind(absence)
                .bind(next_due)
                .fetch_one(&mut *conn)
                .await?;
                let view = view_from_row(&row)?;
                stage(conn, batch, &tenant, &actor, &view, "routine.updated", json!({
                    "routine_id": view.id,
                    "trigger": view.trigger,
                    "next_due_at": view.next_due_at,
                }))
                .await?;
                Ok(view)
            })
        })
        .await
    }

    /// Pause an active routine: no window is due while it is paused.
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] unless it is active.
    pub async fn pause(&self, routine_id: &str) -> Result<RoutineView, RoutineError> {
        self.transition(routine_id, "active", "paused", "routine.paused", None)
            .await
    }

    /// Resume a paused routine from now: the windows missed while paused were not missed
    /// by absence, so the absence policy does not apply to them.
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] unless it is paused.
    pub async fn resume(
        &self,
        routine_id: &str,
        now: DateTime<Utc>,
    ) -> Result<RoutineView, RoutineError> {
        self.transition(routine_id, "paused", "active", "routine.resumed", Some(now))
            .await
    }

    /// Delete a routine (it stays for its history; nothing fires again).
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] when it is already deleted.
    pub async fn delete(&self, routine_id: &str) -> Result<RoutineView, RoutineError> {
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let routine_id = routine_id.to_string();
        self.commit(move |conn, batch| {
            Box::pin(async move {
                let current = lock(conn, &tenant, &routine_id).await?;
                if current.status == "deleted" {
                    return Err(RoutineError::Conflict(format!(
                        "routine {routine_id} is already deleted"
                    )));
                }
                let row = sqlx::query(&format!(
                    "UPDATE routines SET status = 'deleted', next_due_at = NULL \
                     WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
                ))
                .bind(&tenant)
                .bind(&routine_id)
                .fetch_one(&mut *conn)
                .await?;
                let view = view_from_row(&row)?;
                stage(
                    conn,
                    batch,
                    &tenant,
                    &actor,
                    &view,
                    "routine.deleted",
                    json!({ "routine_id": view.id }),
                )
                .await?;
                Ok(view)
            })
        })
        .await
    }

    /// Claim a fire now (`TriggerRoutineNow`), keyed by the command so a retried command
    /// claims the same fire.
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] for a routine that is not active.
    pub async fn trigger_now(
        &self,
        routine_id: &str,
        command_id: &str,
    ) -> Result<Fire, RoutineError> {
        let current = self.get(routine_id).await?;
        if current.status != "active" {
            return Err(RoutineError::Conflict(format!(
                "routine {routine_id} is {} and does not fire",
                current.status
            )));
        }
        let key = format!("routine:{routine_id}:manual:{command_id}");
        let ledger = ledger::FireLedger::new(self.events.pool().clone(), self.tenant_id.clone());
        if let Some(existing) = ledger.by_key(&key).await? {
            return Ok(existing);
        }
        let tenant = self.tenant_id.clone();
        let workspace = current.workspace_id.clone();
        let routine = current.id.clone();
        let claimed = self
            .commit(move |conn, batch| {
                Box::pin(async move {
                    claim_in(
                        conn,
                        batch,
                        &tenant,
                        &workspace,
                        &routine,
                        &key,
                        FireSource::Manual,
                        None,
                        None,
                    )
                    .await?
                    .ok_or_else(|| {
                        RoutineError::Conflict("the fire was claimed concurrently".to_string())
                    })
                })
            })
            .await;
        match claimed {
            Ok(fire) => Ok(fire),
            Err(RoutineError::Conflict(_)) => ledger
                .by_key(&format!("routine:{routine_id}:manual:{command_id}"))
                .await?
                .ok_or_else(|| RoutineError::NotFound(routine_id.to_string())),
            Err(error) => Err(error),
        }
    }

    async fn transition(
        &self,
        routine_id: &str,
        from: &'static str,
        to: &'static str,
        event_type: &'static str,
        resume_from: Option<DateTime<Utc>>,
    ) -> Result<RoutineView, RoutineError> {
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let routine_id = routine_id.to_string();
        self.commit(move |conn, batch| {
            Box::pin(async move {
                let current = lock(conn, &tenant, &routine_id).await?;
                if current.status != from {
                    return Err(RoutineError::Conflict(format!(
                        "routine {routine_id} is {} (not {from})",
                        current.status
                    )));
                }
                let next_due = match resume_from {
                    Some(now) => current
                        .trigger()?
                        .next_due_after(now)
                        .map_err(|error| RoutineError::Validation(error.to_string()))?,
                    None => current.next_due_at,
                };
                let row = sqlx::query(&format!(
                    "UPDATE routines SET status = $3, next_due_at = $4 \
                     WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
                ))
                .bind(&tenant)
                .bind(&routine_id)
                .bind(to)
                .bind(next_due)
                .fetch_one(&mut *conn)
                .await?;
                let view = view_from_row(&row)?;
                stage(
                    conn,
                    batch,
                    &tenant,
                    &actor,
                    &view,
                    event_type,
                    json!({
                        "routine_id": view.id,
                        "status": view.status,
                        "next_due_at": view.next_due_at,
                    }),
                )
                .await?;
                Ok(view)
            })
        })
        .await
    }

    async fn commit<T, F>(&self, mutation: F) -> Result<T, RoutineError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut PgConnection,
                &'a mut EventBatch,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, RoutineError>> + Send + 'a>,
            >,
    {
        let rejection: Arc<Mutex<Option<RoutineError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation(&self.tenant_id, move |conn, batch| {
                Box::pin(async move {
                    match mutation(conn, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: ROUTINES_OWNER,
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(error) => match rejection
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                Some(error) => Err(error),
                None => Err(RoutineError::Event(error)),
            },
        }
    }
}

async fn lock(
    conn: &mut PgConnection,
    tenant_id: &str,
    routine_id: &str,
) -> Result<RoutineView, RoutineError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM routines WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(routine_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| RoutineError::NotFound(routine_id.to_string()))?;
    view_from_row(&row)
}

async fn stage(
    conn: &mut PgConnection,
    batch: &mut EventBatch,
    tenant_id: &str,
    actor: &Actor,
    view: &RoutineView,
    event_type: &str,
    payload: Value,
) -> Result<(), RoutineError> {
    let version = ledger::next_version(conn, tenant_id, &view.id, batch).await?;
    batch.emit(
        EventDraft::new(
            "routine",
            view.id.clone(),
            version,
            EventType::parse(event_type)?,
            CorrelationId::generate(&mut UlidGenerator::new()),
            actor.clone(),
        )
        .with_workspace(view.workspace_id.clone())
        .with_payload(payload),
    );
    Ok(())
}

fn view_from_row(row: &PgRow) -> Result<RoutineView, RoutineError> {
    Ok(RoutineView {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        owner_user_id: row.try_get("owner_user_id")?,
        teammate_id: row.try_get("teammate_id")?,
        trigger: row.try_get("trigger")?,
        objective_template: row.try_get("objective_template")?,
        budget: row.try_get("budget")?,
        notification_prefs: row.try_get("notification_prefs")?,
        absence_policy: row.try_get("absence_policy")?,
        status: row.try_get("status")?,
        last_fired_at: row.try_get("last_fired_at")?,
        next_due_at: row.try_get("next_due_at")?,
        created_at: row.try_get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_needs_a_title_and_a_prompt() {
        assert!(validate_template(&json!({ "title": "daily", "prompt": "summarize" })).is_ok());
        assert!(validate_template(&json!({ "title": " ", "prompt": "x" })).is_err());
        assert!(validate_template(&json!({ "title": "t" })).is_err());
        assert!(validate_template(&json!({ "title": "x".repeat(201), "prompt": "p" })).is_err());
    }
}
