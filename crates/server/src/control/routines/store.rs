//! Routine definitions (`routines`) and their commands.
//!
//! Every change to a definition or status increments `revision`; a change that names the revision it was made against is
//! refused when the routine has moved on (ADP-007, DOMAIN.md §13.8 C). A webhook trigger carries its endpoint and the
//! SecretHandle of its signing secret, minted in the transaction that creates the routine and shown once.

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, CorrelationId, OsEntropy, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use quansio_machine::secrets::{KeyProvider, NewSecret, SecretBroker, SecretError};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

use super::config::{AutomationConfig, PlanLimits, ScriptConfig};
use super::hooks::{endpoint_path, new_endpoint_id, validate_filters};
use super::ledger::{self, claim_in, Fire, FireSource};
use super::script::{validate_plain_prompt, ScriptError, WorkflowScript};
use crate::notify::webhooks::generate_secret;
use crate::scheduler::{AbsencePolicy, RoutineTrigger, RoutineTriggerKind, SchedulerError};

/// Repository path used to attribute a rejected mutation.
pub const ROUTINES_OWNER: &str = "crates/server/src/control/routines";

/// The SecretHandle provider name of an inbound hook's signing secret.
pub const HOOK_SECRET_PROVIDER: &str = "routine_hook";

/// How many upcoming windows of a cron schedule are looked at for the shortest gap.
const GAP_WINDOWS: usize = 16;

const COLUMNS: &str = "id, workspace_id, owner_user_id, teammate_id, trigger, objective_template, \
                       budget, notification_prefs, absence_policy, status, revision, hook_endpoint_id, \
                       hook_secret_handle_id, last_fired_at, next_due_at, created_at, updated_at";

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
    /// A declared number is outside what the configuration allows.
    #[error("{0}")]
    Bounds(String),
    /// The trigger's timezone is neither UTC, a fixed offset nor an IANA name.
    #[error("unknown timezone {0:?}; use UTC, a fixed offset such as +02:00 or an IANA name such as Europe/Paris")]
    UnknownTimezone(String),
    /// No such routine here.
    #[error("routine {0} was not found")]
    NotFound(String),
    /// The routine's state does not allow it.
    #[error("{0}")]
    Conflict(String),
    /// The caller named a revision the routine has moved past.
    #[error("routine {routine_id} is at revision {current}, not {expected}")]
    RevisionConflict {
        /// The routine.
        routine_id: String,
        /// The revision the caller named.
        expected: i32,
        /// The revision it is at.
        current: i32,
    },
    /// A create request id was reused with a different definition.
    #[error("create_request_id {0:?} was used with a different definition")]
    IdempotencyMismatch(String),
    /// A configured limit refuses the change.
    #[error("{message}")]
    Limit {
        /// Which limit.
        limit: &'static str,
        /// What the plan allows.
        allowed: u64,
        /// What the change asked for.
        value: u64,
        /// The sentence.
        message: String,
    },
    /// The deployment has no key provider to seal a hook's signing secret.
    #[error(
        "this deployment has no secret key provider (QUANSIO_SECRETS_KEY_FILE), so an inbound hook \
         cannot be given a signing secret"
    )]
    SigningUnavailable,
    /// The secret broker refused.
    #[error("hook signing secret: {0}")]
    Secret(#[from] SecretError),
}

impl From<ScriptError> for RoutineError {
    fn from(error: ScriptError) -> Self {
        match error {
            ScriptError::Schema(message) => Self::Validation(message),
            ScriptError::Bounds(message) => Self::Bounds(message),
        }
    }
}

impl RoutineError {
    /// DOMAIN.md §15 code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "VALIDATION_SCHEMA",
            Self::Bounds(_) => "VALIDATION_BOUNDS",
            Self::UnknownTimezone(_) => "AUTOMATION_TIMEZONE_UNKNOWN",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT_STATE",
            Self::RevisionConflict { .. } => "CONFLICT_REVISION",
            Self::IdempotencyMismatch(_) => "CONFLICT_IDEMPOTENCY_MISMATCH",
            Self::Limit { .. } => "AUTOMATION_LIMIT_EXCEEDED",
            Self::Event(_) | Self::Database(_) | Self::SigningUnavailable | Self::Secret(_) => {
                "INTERNAL"
            }
        }
    }

    /// The structured detail the code carries, when it has any.
    #[must_use]
    pub fn details(&self) -> Option<Value> {
        match self {
            Self::RevisionConflict {
                routine_id,
                expected,
                current,
            } => Some(json!({
                "routine_id": routine_id,
                "expected_revision": expected,
                "current_revision": current,
            })),
            Self::Limit {
                limit,
                allowed,
                value,
                ..
            } => Some(json!({ "limit": limit, "allowed": allowed, "value": value })),
            Self::UnknownTimezone(timezone) => Some(json!({ "timezone": timezone })),
            _ => None,
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
    /// `{kind, spec, timezone, filters?}`.
    pub trigger: Value,
    /// `{title, prompt, workflow?}`.
    pub objective_template: Value,
    /// Budget limits.
    pub budget: Value,
    /// Routine-level notification preferences.
    pub notification_prefs: Value,
    /// `skip`, `queue` or `catch_up_once`.
    pub absence_policy: String,
    /// `active`, `paused` or `deleted`.
    pub status: String,
    /// Changes to the definition or status so far; 1 at creation.
    pub revision: i32,
    /// The name of the inbound hook's endpoint, for a webhook trigger.
    pub hook_endpoint_id: Option<String>,
    /// The SecretHandle holding its signing secret (an identifier, never the material).
    pub hook_secret_handle_id: Option<String>,
    /// When it last fired.
    pub last_fired_at: Option<DateTime<Utc>>,
    /// When it is next due (schedule triggers).
    pub next_due_at: Option<DateTime<Utc>>,
    /// Created.
    pub created_at: DateTime<Utc>,
}

impl RoutineView {
    /// The projection's spelling. The secret is never part of it, and neither is the SecretHandle.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let hook = self.hook_endpoint_id.as_ref().map(|endpoint| {
            json!({
                "endpoint_id": endpoint,
                "path": endpoint_path(endpoint),
                "secret": if self.hook_secret_handle_id.is_some() { json!("set") } else { Value::Null },
            })
        });
        let script = self
            .objective_template
            .get("workflow")
            .filter(|workflow| !workflow.is_null())
            .and_then(|workflow| WorkflowScript::parse(workflow).ok())
            .map(|script| script.outline());
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
            "revision": self.revision,
            "hook": hook,
            "script": script,
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

    /// The routine's workflow script, when it has one.
    ///
    /// # Errors
    /// Returns [`RoutineError::Validation`] for a stored script that does not parse.
    pub fn script(&self) -> Result<Option<WorkflowScript>, RoutineError> {
        match self.objective_template.get("workflow") {
            None | Some(Value::Null) => Ok(None),
            Some(workflow) => Ok(Some(WorkflowScript::parse(workflow)?)),
        }
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
    /// `{title, prompt, workflow?}`.
    pub objective_template: Value,
    /// Budget.
    pub budget: Value,
    /// Preferences.
    pub notification_prefs: Value,
    /// Absence policy.
    pub absence_policy: AbsencePolicy,
    /// The client's name for this create: a second one with the same definition returns the first.
    pub create_request_id: Option<String>,
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
    /// The revision the caller made the change against; a routine at another revision is refused.
    pub expected_revision: Option<i32>,
}

/// What creating a routine did.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedRoutine {
    /// The routine.
    pub view: RoutineView,
    /// The hook's signing secret, shown this once; `None` for a routine without a hook and for a replayed create.
    pub secret: Option<String>,
    /// Whether this was a repeat of an earlier create with the same `create_request_id`.
    pub replayed: bool,
}

/// What changing a routine did.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangedRoutine {
    /// The routine.
    pub view: RoutineView,
    /// A new signing secret, shown this once, when the change gave the routine a hook or rotated it.
    pub secret: Option<String>,
}

/// Refuse a template without a title and a prompt of sane size, a prompt naming a variable it does not have, and a
/// workflow script that breaks the contract.
///
/// # Errors
/// Returns [`RoutineError::Validation`] or [`RoutineError::Bounds`].
pub fn validate_template(template: &Value, script: &ScriptConfig) -> Result<(), RoutineError> {
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
    validate_plain_prompt(prompt)?;
    if let Some(object) = template.as_object() {
        if let Some(unknown) = object
            .keys()
            .find(|key| !matches!(key.as_str(), "title" | "prompt" | "workflow" | "mode"))
        {
            return Err(RoutineError::Validation(format!(
                "objective_template has no field {unknown:?}"
            )));
        }
    }
    // `mode` is stored only as `observe` (`act` is the default and absent), so nothing else is a mode.
    if template
        .get("mode")
        .is_some_and(|mode| mode.as_str() != Some("observe"))
    {
        return Err(RoutineError::Validation(
            "objective_template.mode is `observe` or absent".to_string(),
        ));
    }
    match template.get("workflow") {
        None | Some(Value::Null) => {}
        Some(workflow) => {
            WorkflowScript::parse(workflow)?.validate(script)?;
        }
    }
    Ok(())
}

pub(super) fn trigger_error(error: SchedulerError) -> RoutineError {
    match error {
        SchedulerError::UnknownTimezone(timezone) => RoutineError::UnknownTimezone(timezone),
        other => RoutineError::Validation(other.to_string()),
    }
}

pub(super) fn validate_trigger(trigger: &RoutineTrigger) -> Result<(), RoutineError> {
    trigger.validate().map_err(trigger_error)?;
    match trigger.kind {
        RoutineTriggerKind::Event => super::triggers::validate_event_spec(&trigger.spec)?,
        RoutineTriggerKind::Webhook => {
            validate_filters(&trigger.filters).map_err(RoutineError::Validation)?;
        }
        RoutineTriggerKind::Cron | RoutineTriggerKind::Interval => {}
    }
    if trigger.kind != RoutineTriggerKind::Webhook && !trigger.filters.is_empty() {
        return Err(RoutineError::Validation(
            "filters belong to a webhook trigger".to_string(),
        ));
    }
    Ok(())
}

/// Refuse a schedule that fires closer together than the plan allows.
pub(super) fn check_interval(
    trigger: &RoutineTrigger,
    limits: &PlanLimits,
    now: DateTime<Utc>,
) -> Result<(), RoutineError> {
    let Some(gap) = trigger
        .shortest_gap(now, GAP_WINDOWS)
        .map_err(trigger_error)?
    else {
        return Ok(());
    };
    let seconds = u64::try_from(gap.num_seconds()).unwrap_or(0);
    if seconds < limits.min_interval_seconds {
        return Err(RoutineError::Limit {
            limit: "min_interval_seconds",
            allowed: limits.min_interval_seconds,
            value: seconds,
            message: format!(
                "this schedule fires every {seconds} s at its closest; the plan's minimum is {} s",
                limits.min_interval_seconds
            ),
        });
    }
    Ok(())
}

/// The limits that apply to the tenant, read inside the caller's transaction.
async fn plan_limits(
    conn: &mut PgConnection,
    config: &AutomationConfig,
    tenant_id: &str,
) -> Result<PlanLimits, RoutineError> {
    let personal: Option<bool> = sqlx::query_scalar("SELECT personal FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(config.limits_for(tenant_id, personal.unwrap_or(false)))
}

/// Refuse another active routine in a workspace that is at its plan's limit.
async fn check_enabled(
    conn: &mut PgConnection,
    limits: &PlanLimits,
    tenant_id: &str,
    workspace_id: &str,
) -> Result<(), RoutineError> {
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM routines WHERE tenant_id = $1 AND workspace_id = $2 AND status = 'active'",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .fetch_one(&mut *conn)
    .await?;
    let active = u64::try_from(active).unwrap_or(0);
    if active >= limits.max_enabled_triggers {
        return Err(RoutineError::Limit {
            limit: "max_enabled_triggers",
            allowed: limits.max_enabled_triggers,
            value: active + 1,
            message: format!(
                "this workspace already has {active} active routines; the plan's limit is {}",
                limits.max_enabled_triggers
            ),
        });
    }
    Ok(())
}

/// Serialise the changes that count a workspace's routines, so two creates cannot both pass the limit.
async fn lock_workspace(
    conn: &mut PgConnection,
    tenant_id: &str,
    workspace_id: &str,
) -> Result<(), RoutineError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("routines:{tenant_id}:{workspace_id}"))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

fn check_expected(current: &RoutineView, expected: Option<i32>) -> Result<(), RoutineError> {
    match expected {
        Some(expected) if expected != current.revision => Err(RoutineError::RevisionConflict {
            routine_id: current.id.clone(),
            expected,
            current: current.revision,
        }),
        _ => Ok(()),
    }
}

/// The digest of everything a create asked for, so the same `create_request_id` with a different definition is told apart.
fn definition_digest(new: &NewRoutine, trigger: &Value) -> String {
    let canonical = json!({
        "workspace_id": new.workspace_id,
        "owner_user_id": new.owner_user_id,
        "teammate_id": new.teammate_id,
        "trigger": trigger,
        "objective_template": new.objective_template,
        "budget": new.budget,
        "notification_prefs": new.notification_prefs,
        "absence_policy": new.absence_policy.as_db_str(),
    });
    hex::encode(Sha256::digest(canonical.to_string().as_bytes()))
}

/// The routine definitions for one tenant.
#[derive(Clone)]
pub struct RoutineStore {
    events: EventStore,
    tenant_id: String,
    actor: Actor,
    config: Arc<AutomationConfig>,
    keys: Option<Arc<dyn KeyProvider>>,
}

impl std::fmt::Debug for RoutineStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutineStore")
            .field("tenant_id", &self.tenant_id)
            .field("actor", &self.actor)
            .finish_non_exhaustive()
    }
}

impl RoutineStore {
    /// A store acting as `actor` in one tenant, under the shipped `config/automation.yaml`.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>, actor: Actor) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
            actor,
            config: Arc::new(AutomationConfig::shipped()),
            keys: None,
        }
    }

    /// Use `config` instead of the shipped configuration.
    #[must_use]
    pub fn with_config(mut self, config: Arc<AutomationConfig>) -> Self {
        self.config = config;
        self
    }

    /// Seal inbound hooks' signing secrets under `keys`.
    #[must_use]
    pub fn with_key_provider(mut self, keys: Option<Arc<dyn KeyProvider>>) -> Self {
        self.keys = keys;
        self
    }

    /// The configuration in force.
    #[must_use]
    pub fn config(&self) -> &Arc<AutomationConfig> {
        &self.config
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

    async fn find_by_request(
        &self,
        workspace_id: &str,
        create_request_id: &str,
    ) -> Result<Option<(RoutineView, Option<String>)>, RoutineError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS}, create_digest FROM routines \
             WHERE tenant_id = $1 AND workspace_id = $2 AND create_request_id = $3"
        ))
        .bind(&self.tenant_id)
        .bind(workspace_id)
        .bind(create_request_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        match row {
            Some(row) => Ok(Some((view_from_row(&row)?, row.try_get("create_digest")?))),
            None => Ok(None),
        }
    }

    /// Create a routine; a schedule trigger's first window is computed now. A webhook trigger gets its endpoint and its
    /// signing secret, which is returned once.
    ///
    /// A `create_request_id` that an earlier create used with the same definition returns that routine (`replayed`).
    ///
    /// # Errors
    /// [`RoutineError::Validation`] for a malformed trigger or template, [`RoutineError::UnknownTimezone`],
    /// [`RoutineError::Limit`], [`RoutineError::IdempotencyMismatch`] and [`RoutineError::SigningUnavailable`].
    pub async fn create(
        &self,
        new: NewRoutine,
        now: DateTime<Utc>,
    ) -> Result<CreatedRoutine, RoutineError> {
        validate_trigger(&new.trigger)?;
        validate_template(&new.objective_template, &self.config.script)?;
        if new.trigger.kind == RoutineTriggerKind::Webhook && self.keys.is_none() {
            return Err(RoutineError::SigningUnavailable);
        }
        let next_due = new.trigger.initial_due(now).map_err(trigger_error)?;
        let trigger = serde_json::to_value(&new.trigger)
            .map_err(|error| RoutineError::Validation(error.to_string()))?;
        let digest = definition_digest(&new, &trigger);
        if let Some(request_id) = new.create_request_id.as_deref() {
            if let Some(replay) = self.replay(&new.workspace_id, request_id, &digest).await? {
                return Ok(replay);
            }
        }
        let id = CanonicalId::generate(Prefix::Routine, &mut UlidGenerator::new()).to_string();
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let config = Arc::clone(&self.config);
        let keys = self.keys.clone();
        let request_id = new.create_request_id.clone();
        let raced = (new.workspace_id.clone(), new.create_request_id.clone());
        let created = self
            .commit({
                let digest = digest.clone();
                move |conn, batch| {
                    Box::pin(async move {
                        lock_workspace(conn, &tenant, &new.workspace_id).await?;
                        let limits = plan_limits(conn, &config, &tenant).await?;
                        check_interval(&new.trigger, &limits, now)?;
                        check_enabled(conn, &limits, &tenant, &new.workspace_id).await?;
                        let (endpoint, handle, secret) = if new.trigger.kind
                            == RoutineTriggerKind::Webhook
                        {
                            let provider = keys.as_deref().ok_or(RoutineError::SigningUnavailable)?;
                            let minted =
                                mint_hook(conn, provider, &tenant, &new.workspace_id, &id).await?;
                            (Some(minted.0), Some(minted.1), Some(minted.2))
                        } else {
                            (None, None, None)
                        };
                        let row = sqlx::query(&format!(
                            "INSERT INTO routines (id, tenant_id, workspace_id, owner_user_id, teammate_id, trigger, \
                                                   objective_template, budget, notification_prefs, absence_policy, \
                                                   status, next_due_at, create_request_id, create_digest, \
                                                   hook_endpoint_id, hook_secret_handle_id) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'active', $11, $12, $13, $14, $15) \
                             RETURNING {COLUMNS}"
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
                        .bind(&request_id)
                        .bind(&digest)
                        .bind(&endpoint)
                        .bind(&handle)
                        .fetch_one(&mut *conn)
                        .await?;
                        let view = view_from_row(&row)?;
                        stage(conn, batch, &tenant, &actor, &view, "routine.created", json!({
                            "routine_id": view.id,
                            "trigger": view.trigger,
                            "revision": view.revision,
                            "next_due_at": view.next_due_at,
                        }))
                        .await?;
                        Ok(CreatedRoutine {
                            view,
                            secret,
                            replayed: false,
                        })
                    })
                }
            })
            .await;
        match created {
            Ok(created) => Ok(created),
            // Two creates with one request id raced: the loser finds the winner's routine.
            Err(RoutineError::Database(sqlx::Error::Database(error)))
                if error.constraint() == Some("routines_create_request_idx") =>
            {
                let (workspace_id, Some(request_id)) = raced else {
                    return Err(RoutineError::Database(sqlx::Error::Database(error)));
                };
                self.replay(&workspace_id, &request_id, &digest)
                    .await?
                    .ok_or(RoutineError::IdempotencyMismatch(request_id))
            }
            Err(error) => Err(error),
        }
    }

    async fn replay(
        &self,
        workspace_id: &str,
        request_id: &str,
        digest: &str,
    ) -> Result<Option<CreatedRoutine>, RoutineError> {
        match self.find_by_request(workspace_id, request_id).await? {
            None => Ok(None),
            Some((view, stored)) if stored.as_deref() == Some(digest) => Ok(Some(CreatedRoutine {
                view,
                secret: None,
                replayed: true,
            })),
            Some(_) => Err(RoutineError::IdempotencyMismatch(request_id.to_string())),
        }
    }

    /// Change a routine that is not deleted; a new trigger recomputes the next window. A change that names an
    /// `expected_revision` is refused when the routine is at another one.
    ///
    /// # Errors
    /// [`RoutineError::Conflict`] for a deleted routine, [`RoutineError::RevisionConflict`] for a stale revision.
    pub async fn update(
        &self,
        routine_id: &str,
        change: RoutineChange,
        now: DateTime<Utc>,
    ) -> Result<ChangedRoutine, RoutineError> {
        if let Some(trigger) = &change.trigger {
            validate_trigger(trigger)?;
            if trigger.kind == RoutineTriggerKind::Webhook && self.keys.is_none() {
                return Err(RoutineError::SigningUnavailable);
            }
        }
        if let Some(template) = &change.objective_template {
            validate_template(template, &self.config.script)?;
        }
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let routine_id = routine_id.to_string();
        let config = Arc::clone(&self.config);
        let keys = self.keys.clone();
        self.commit(move |conn, batch| {
            Box::pin(async move {
                let current = lock(conn, &tenant, &routine_id).await?;
                if current.status == "deleted" {
                    return Err(RoutineError::Conflict(format!("routine {routine_id} is deleted")));
                }
                check_expected(&current, change.expected_revision)?;
                let trigger_changed = change.trigger.is_some();
                if let Some(trigger) = &change.trigger {
                    let limits = plan_limits(conn, &config, &tenant).await?;
                    check_interval(trigger, &limits, now)?;
                }
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
                        .map_err(trigger_error)?
                        .flatten()
                } else {
                    current.next_due_at
                };
                // A routine whose trigger becomes a webhook gets an endpoint and a secret; one whose trigger stops
                // being one gives them up, so the old endpoint answers nothing.
                let becomes_hook = change
                    .trigger
                    .as_ref()
                    .is_some_and(|trigger| trigger.kind == RoutineTriggerKind::Webhook);
                let stops_being_hook = change
                    .trigger
                    .as_ref()
                    .is_some_and(|trigger| trigger.kind != RoutineTriggerKind::Webhook);
                let (mut endpoint, mut handle, mut secret) = (
                    current.hook_endpoint_id.clone(),
                    current.hook_secret_handle_id.clone(),
                    None,
                );
                if becomes_hook && endpoint.is_none() {
                    let provider = keys.as_deref().ok_or(RoutineError::SigningUnavailable)?;
                    let minted =
                        mint_hook(conn, provider, &tenant, &current.workspace_id, &current.id).await?;
                    endpoint = Some(minted.0);
                    handle = Some(minted.1);
                    secret = Some(minted.2);
                } else if stops_being_hook {
                    if let Some(old) = handle.take() {
                        SecretBroker::revoke(conn, &tenant, &old).await?;
                    }
                    endpoint = None;
                }
                let teammate = match change.teammate_id {
                    Some(value) => value,
                    None => current.teammate_id.clone(),
                };
                let absence = change
                    .absence_policy
                    .map_or_else(|| current.absence_policy.clone(), |policy| policy.as_db_str().to_string());
                let row = sqlx::query(&format!(
                    "UPDATE routines SET trigger = $3, objective_template = $4, teammate_id = $5, budget = $6, \
                            notification_prefs = $7, absence_policy = $8, next_due_at = $9, \
                            hook_endpoint_id = $10, hook_secret_handle_id = $11, revision = revision + 1 \
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
                .bind(&endpoint)
                .bind(&handle)
                .fetch_one(&mut *conn)
                .await?;
                let view = view_from_row(&row)?;
                stage(conn, batch, &tenant, &actor, &view, "routine.updated", json!({
                    "routine_id": view.id,
                    "trigger": view.trigger,
                    "revision": view.revision,
                    "next_due_at": view.next_due_at,
                }))
                .await?;
                Ok(ChangedRoutine { view, secret })
            })
        })
        .await
    }

    /// Pause an active routine: no window is due while it is paused.
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] unless it is active, [`RoutineError::RevisionConflict`] for a stale revision.
    pub async fn pause(
        &self,
        routine_id: &str,
        expected_revision: Option<i32>,
    ) -> Result<RoutineView, RoutineError> {
        self.transition(
            routine_id,
            "active",
            "paused",
            "routine.paused",
            None,
            expected_revision,
        )
        .await
    }

    /// Resume a paused routine from now: the windows missed while paused were not missed
    /// by absence, so the absence policy does not apply to them.
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] unless it is paused, [`RoutineError::Limit`] when the workspace is at its
    /// limit of active routines, [`RoutineError::RevisionConflict`] for a stale revision.
    pub async fn resume(
        &self,
        routine_id: &str,
        now: DateTime<Utc>,
        expected_revision: Option<i32>,
    ) -> Result<RoutineView, RoutineError> {
        self.transition(
            routine_id,
            "paused",
            "active",
            "routine.resumed",
            Some(now),
            expected_revision,
        )
        .await
    }

    /// Delete a routine (it stays for its history; nothing fires again). Its hook, if any, stops answering and its
    /// signing secret is revoked.
    ///
    /// # Errors
    /// Returns [`RoutineError::Conflict`] when it is already deleted, [`RoutineError::RevisionConflict`] for a stale
    /// revision.
    pub async fn delete(
        &self,
        routine_id: &str,
        expected_revision: Option<i32>,
    ) -> Result<RoutineView, RoutineError> {
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
                check_expected(&current, expected_revision)?;
                if let Some(handle) = &current.hook_secret_handle_id {
                    SecretBroker::revoke(conn, &tenant, handle).await?;
                }
                let row = sqlx::query(&format!(
                    "UPDATE routines SET status = 'deleted', next_due_at = NULL, revision = revision + 1 \
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
                    json!({ "routine_id": view.id, "revision": view.revision }),
                )
                .await?;
                Ok(view)
            })
        })
        .await
    }

    /// Replace a hook's signing secret: the old one stops working at once and the new one is returned, this once.
    ///
    /// # Errors
    /// [`RoutineError::Conflict`] for a routine with no hook, [`RoutineError::RevisionConflict`] for a stale revision,
    /// [`RoutineError::SigningUnavailable`] without a key provider.
    pub async fn rotate_hook(
        &self,
        routine_id: &str,
        expected_revision: Option<i32>,
    ) -> Result<ChangedRoutine, RoutineError> {
        let keys = self.keys.clone().ok_or(RoutineError::SigningUnavailable)?;
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let routine_id = routine_id.to_string();
        self.commit(move |conn, batch| {
            Box::pin(async move {
                let current = lock(conn, &tenant, &routine_id).await?;
                if current.status == "deleted" {
                    return Err(RoutineError::Conflict(format!("routine {routine_id} is deleted")));
                }
                check_expected(&current, expected_revision)?;
                let Some(handle) = current.hook_secret_handle_id.clone() else {
                    return Err(RoutineError::Conflict(format!(
                        "routine {routine_id} has no inbound hook to rotate"
                    )));
                };
                let secret = generate_secret(&OsEntropy);
                SecretBroker::rotate(
                    conn,
                    keys.as_ref(),
                    &tenant,
                    &handle,
                    secret.as_bytes(),
                    &OsEntropy,
                )
                .await?;
                let row = sqlx::query(&format!(
                    "UPDATE routines SET revision = revision + 1 WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
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
                    "routine.hook_rotated",
                    json!({ "routine_id": view.id, "revision": view.revision, "secret_handle_id": handle }),
                )
                .await?;
                Ok(ChangedRoutine {
                    view,
                    secret: Some(secret),
                })
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
        expected_revision: Option<i32>,
    ) -> Result<RoutineView, RoutineError> {
        let tenant = self.tenant_id.clone();
        let actor = self.actor.clone();
        let routine_id = routine_id.to_string();
        let config = Arc::clone(&self.config);
        self.commit(move |conn, batch| {
            Box::pin(async move {
                let current = lock(conn, &tenant, &routine_id).await?;
                if current.status != from {
                    return Err(RoutineError::Conflict(format!(
                        "routine {routine_id} is {} (not {from})",
                        current.status
                    )));
                }
                check_expected(&current, expected_revision)?;
                if to == "active" {
                    lock_workspace(conn, &tenant, &current.workspace_id).await?;
                    let limits = plan_limits(conn, &config, &tenant).await?;
                    check_enabled(conn, &limits, &tenant, &current.workspace_id).await?;
                }
                let next_due = match resume_from {
                    Some(now) => current
                        .trigger()?
                        .next_due_after(now)
                        .map_err(trigger_error)?,
                    None => current.next_due_at,
                };
                let row = sqlx::query(&format!(
                    "UPDATE routines SET status = $3, next_due_at = $4, revision = revision + 1 \
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
                        "revision": view.revision,
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

/// Mint a hook: its endpoint, and its signing secret sealed as a SecretHandle in the caller's transaction.
async fn mint_hook(
    conn: &mut PgConnection,
    provider: &dyn KeyProvider,
    tenant_id: &str,
    workspace_id: &str,
    routine_id: &str,
) -> Result<(String, String, String), RoutineError> {
    let endpoint = new_endpoint_id(&OsEntropy);
    let handle = CanonicalId::generate(Prefix::SecretHandle, &mut UlidGenerator::new()).to_string();
    let secret = generate_secret(&OsEntropy);
    SecretBroker::register(
        conn,
        provider,
        &NewSecret {
            id: &handle,
            tenant_id,
            workspace_id: Some(workspace_id),
            provider: HOOK_SECRET_PROVIDER,
            label: &format!("inbound hook of {routine_id}"),
            expires_at: None,
        },
        secret.as_bytes(),
        &OsEntropy,
    )
    .await?;
    Ok((endpoint, handle, secret))
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
        revision: row.try_get("revision")?,
        hook_endpoint_id: row.try_get("hook_endpoint_id")?,
        hook_secret_handle_id: row.try_get("hook_secret_handle_id")?,
        last_fired_at: row.try_get("last_fired_at")?,
        next_due_at: row.try_get("next_due_at")?,
        created_at: row.try_get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script_config() -> ScriptConfig {
        AutomationConfig::shipped().script
    }

    #[test]
    fn a_template_needs_a_title_and_a_prompt() {
        let config = script_config();
        assert!(
            validate_template(&json!({ "title": "daily", "prompt": "summarize" }), &config).is_ok()
        );
        assert!(validate_template(&json!({ "title": " ", "prompt": "x" }), &config).is_err());
        assert!(validate_template(&json!({ "title": "t" }), &config).is_err());
        assert!(
            validate_template(&json!({ "title": "x".repeat(201), "prompt": "p" }), &config)
                .is_err()
        );
        // The prompt names only variables it has, and the template has no other fields.
        assert!(validate_template(
            &json!({ "title": "t", "prompt": "see {{payload}}" }),
            &config
        )
        .is_ok());
        assert!(validate_template(
            &json!({ "title": "t", "prompt": "see {{secret}}" }),
            &config
        )
        .is_err());
        assert!(validate_template(
            &json!({ "title": "t", "prompt": "p", "tools": ["fs.write"] }),
            &config
        )
        .is_err());
    }

    #[test]
    fn a_stale_revision_is_a_typed_conflict_that_names_the_current_one() {
        let view = RoutineView {
            id: "rtn_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
            workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
            owner_user_id: "usr_01J8Z3K6F1N8VQ2X5W9Y0ABCDE".to_string(),
            teammate_id: None,
            trigger: json!({}),
            objective_template: json!({}),
            budget: json!({}),
            notification_prefs: json!({}),
            absence_policy: "skip".to_string(),
            status: "active".to_string(),
            revision: 4,
            hook_endpoint_id: None,
            hook_secret_handle_id: None,
            last_fired_at: None,
            next_due_at: None,
            created_at: Utc::now(),
        };
        assert!(
            check_expected(&view, None).is_ok(),
            "no expectation, no check"
        );
        assert!(check_expected(&view, Some(4)).is_ok());
        let error = check_expected(&view, Some(3)).expect_err("stale");
        assert_eq!(error.code(), "CONFLICT_REVISION");
        assert_eq!(
            error.details().expect("details"),
            json!({ "routine_id": view.id, "expected_revision": 3, "current_revision": 4 })
        );
    }
}
