//! The durable teammate registry (APP-014) over `teammates`, with its AgentThread.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, CorrelationId, Digest, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore, EventType};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::{
    templates, validate_availability, validate_memory_scope, validate_needs, TeammateError,
};
use crate::audit::{append_in, AuditDraft};
use crate::runtime::agents::{
    AgentKind, AgentStore, AgentThread, AgentThreadStatus, NewAgentThread,
};
use crate::runtime::state_machine::RuntimeIdentity;

/// Repository path of this module's canonical owner.
pub const TEAMMATE_OWNER: &str = "crates/server/src/control/teammates";

const COLUMNS: &str = "id, tenant_id, workspace_id, name, persona, standing_instructions, \
                       default_capability_needs, memory_scope, status, template_id, \
                       model_route_preference, default_execution_target_id, availability, \
                       version, created_at, updated_at";

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, TeammateError>> + Send + 'a>>;

/// Who is changing a definition.
#[derive(Debug, Clone)]
pub struct TeammateAdmin {
    /// Owning tenant.
    pub tenant_id: String,
    /// Who acted.
    pub actor: Actor,
    /// Request correlation.
    pub correlation_id: CorrelationId,
}

/// A persisted `teammates` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Teammate {
    /// `agt_…` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Display name.
    pub name: String,
    /// Persona text (never a grant).
    pub persona: String,
    /// Standing instructions (never a grant).
    pub standing_instructions: String,
    /// Effect classes the teammate needs; narrows the policy's grants, never widens them.
    pub default_capability_needs: Vec<String>,
    /// `user | workspace | teammate`.
    pub memory_scope: String,
    /// `active | archived`.
    pub status: String,
    /// The template it was instantiated from, if any.
    pub template_id: Option<String>,
    /// Preferred model route (INT-003 decides).
    pub model_route_preference: Option<String>,
    /// Execution target its runs bind by default.
    pub default_execution_target_id: Option<String>,
    /// `available | away`.
    pub availability: String,
    /// Bumped by every configuration change.
    pub version: i32,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

impl Teammate {
    /// SHA-256 over the definition's authority-relevant fields: what a run's projection
    /// input records, so a changed definition is a changed input.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of_canonical_json(
            &json!({
                "id": self.id.to_string(),
                "version": self.version,
                "default_capability_needs": self.default_capability_needs,
                "memory_scope": self.memory_scope,
                "status": self.status,
                "availability": self.availability,
            })
            .to_string(),
        )
    }

    /// The wire shape.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.to_string(),
            "workspace_id": self.workspace_id,
            "name": self.name,
            "persona": self.persona,
            "standing_instructions": self.standing_instructions,
            "default_capability_needs": self.default_capability_needs,
            "memory_scope": self.memory_scope,
            "status": self.status,
            "template_id": self.template_id,
            "model_route_preference": self.model_route_preference,
            "default_execution_target_id": self.default_execution_target_id,
            "availability": self.availability,
            "version": self.version,
            "definition_digest": self.digest().to_string(),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

/// What `CreateTeammate` carries: a template, or the fields themselves.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewTeammate {
    /// Instantiate this shipped template first; explicit fields override it.
    pub template_id: Option<String>,
    /// Display name.
    pub name: Option<String>,
    /// Persona text.
    pub persona: Option<String>,
    /// Standing instructions.
    pub standing_instructions: Option<String>,
    /// Effect classes needed.
    pub default_capability_needs: Option<Vec<String>>,
    /// Memory scope.
    pub memory_scope: Option<String>,
    /// Model route preference.
    pub model_route_preference: Option<String>,
    /// Default execution target.
    pub default_execution_target_id: Option<String>,
    /// Availability.
    pub availability: Option<String>,
}

/// What `UpdateTeammate` carries: every field optional, `Some` replaces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TeammateChange {
    /// New name.
    pub name: Option<String>,
    /// New persona.
    pub persona: Option<String>,
    /// New standing instructions.
    pub standing_instructions: Option<String>,
    /// New needs (the whole list).
    pub default_capability_needs: Option<Vec<String>>,
    /// New memory scope.
    pub memory_scope: Option<String>,
    /// New route preference (`Some(None)` clears it).
    pub model_route_preference: Option<Option<String>>,
    /// New default target (`Some(None)` clears it).
    pub default_execution_target_id: Option<Option<String>>,
    /// New availability.
    pub availability: Option<String>,
}

/// What the turn loop reads at turn time: the definition behind an AgentThread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnDefinition {
    /// The teammate.
    pub teammate_id: String,
    /// Its name.
    pub name: String,
    /// Persona text.
    pub persona: String,
    /// Standing instructions.
    pub standing_instructions: String,
    /// Route preference.
    pub model_route_preference: Option<String>,
    /// Definition version the turn ran under.
    pub version: i32,
}

/// The durable teammate registry.
#[derive(Debug, Clone)]
pub struct TeammateStore {
    events: EventStore,
    pool: PgPool,
}

impl TeammateStore {
    /// A store writing through `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            events: EventStore::new(pool.clone()),
            pool,
        }
    }

    /// Every teammate of a workspace, archived ones included (history, not absence).
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn list(
        &self,
        tenant_id: &str,
        workspace_id: &str,
    ) -> Result<Vec<Teammate>, TeammateError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM teammates WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at, id"
        ))
        .bind(tenant_id)
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(teammate_from_row).collect()
    }

    /// One teammate.
    ///
    /// # Errors
    /// [`TeammateError::NotFound`].
    pub async fn get(
        &self,
        tenant_id: &str,
        teammate_id: &CanonicalId,
    ) -> Result<Teammate, TeammateError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM teammates WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(tenant_id)
        .bind(teammate_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref()
            .map(teammate_from_row)
            .transpose()?
            .ok_or_else(|| TeammateError::NotFound(teammate_id.to_string()))
    }

    /// The definition behind an AgentThread, as the turn loop reads it at turn time; `None`
    /// when the thread has no definition (a worker, or a pre-definition teammate thread).
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn turn_definition(
        &self,
        tenant_id: &str,
        agent_thread_id: &str,
    ) -> Result<Option<TurnDefinition>, TeammateError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let row = sqlx::query(
            "SELECT t.id, t.name, t.persona, t.standing_instructions, t.model_route_preference, t.version \
             FROM agent_threads a JOIN teammates t ON t.id = a.definition_id AND t.tenant_id = a.tenant_id \
             WHERE a.tenant_id = $1 AND a.id = $2",
        )
        .bind(tenant_id)
        .bind(agent_thread_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.map(|row| TurnDefinition {
            teammate_id: row.get("id"),
            name: row.get("name"),
            persona: row.get::<Option<String>, _>("persona").unwrap_or_default(),
            standing_instructions: row
                .get::<Option<String>, _>("standing_instructions")
                .unwrap_or_default(),
            model_route_preference: row.get("model_route_preference"),
            version: row.get("version"),
        }))
    }

    /// The teammate a message to the workspace goes to when none is named: the oldest
    /// active, available one.
    ///
    /// # Errors
    /// Returns the database error.
    pub async fn default_for(
        &self,
        tenant_id: &str,
        workspace_id: &str,
    ) -> Result<Option<Teammate>, TeammateError> {
        Ok(self
            .list(tenant_id, workspace_id)
            .await?
            .into_iter()
            .find(|teammate| teammate.status == "active" && teammate.availability == "available"))
    }

    /// The teammate's AgentThread in its workspace, if provisioned.
    ///
    /// # Errors
    /// Returns the runtime's error.
    pub async fn thread_of(
        &self,
        identity: &RuntimeIdentity,
        teammate: &Teammate,
    ) -> Result<Option<AgentThread>, TeammateError> {
        let agents = AgentStore::new(self.pool.clone(), identity.clone())?;
        Ok(agents
            .list_threads(&teammate.workspace_id)
            .await?
            .into_iter()
            .find(|thread| thread.definition_id.as_ref() == Some(&teammate.id)))
    }

    /// Create a teammate — from a template, or from explicit fields — and provision its
    /// ACTIVE AgentThread in the workspace (RUN-002).
    ///
    /// # Errors
    /// [`TeammateError::Schema`] for a field outside its vocabulary or a missing name,
    /// [`TeammateError::NotFound`] for an unknown template.
    pub async fn create(
        &self,
        admin: &TeammateAdmin,
        identity: &RuntimeIdentity,
        workspace_id: &str,
        new: NewTeammate,
    ) -> Result<(Teammate, AgentThread), TeammateError> {
        let template = match new.template_id.as_deref() {
            Some(id) => Some(
                templates()?
                    .into_iter()
                    .find(|template| template.id == id)
                    .ok_or_else(|| TeammateError::NotFound(format!("template {id}")))?,
            ),
            None => None,
        };
        let name = new
            .name
            .or_else(|| template.as_ref().map(|template| template.name.clone()))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty() && name.len() <= 120)
            .ok_or_else(|| {
                TeammateError::Schema("name is required (1..=120 characters)".to_string())
            })?;
        let persona = new
            .persona
            .or_else(|| template.as_ref().map(|template| template.persona.clone()))
            .unwrap_or_default();
        let standing_instructions = new
            .standing_instructions
            .or_else(|| {
                template
                    .as_ref()
                    .map(|template| template.standing_instructions.clone())
            })
            .unwrap_or_default();
        let needs = new
            .default_capability_needs
            .or_else(|| {
                template
                    .as_ref()
                    .map(|template| template.default_capability_needs.clone())
            })
            .unwrap_or_default();
        validate_needs(&needs)?;
        let memory_scope = new
            .memory_scope
            .or_else(|| {
                template
                    .as_ref()
                    .map(|template| template.memory_scope.clone())
            })
            .unwrap_or_else(|| "workspace".to_string());
        validate_memory_scope(&memory_scope)?;
        let availability = new
            .availability
            .or_else(|| {
                template
                    .as_ref()
                    .map(|template| template.availability.clone())
            })
            .unwrap_or_else(|| "available".to_string());
        validate_availability(&availability)?;
        let model_route_preference = new.model_route_preference.or_else(|| {
            template
                .as_ref()
                .and_then(|template| template.model_route_preference.clone())
        });
        let default_execution_target_id = new.default_execution_target_id;
        if standing_instructions.len() > 20_000 || persona.len() > 4_000 {
            return Err(TeammateError::Schema(
                "persona is at most 4000 and standing instructions at most 20000 characters"
                    .to_string(),
            ));
        }
        let template_id = template.map(|template| template.id);
        let tenant = admin.tenant_id.clone();
        let tenant_scope = tenant.clone();
        let admin = admin.clone();
        let workspace = workspace_id.to_string();
        let teammate = self
            .commit(&tenant_scope, move |tx, batch| {
                Box::pin(async move {
                    let id = CanonicalId::generate(Prefix::Teammate, &mut UlidGenerator::new());
                    sqlx::query(
                        "INSERT INTO teammates (id, tenant_id, workspace_id, name, persona, standing_instructions, \
                         default_capability_needs, memory_scope, status, template_id, model_route_preference, \
                         default_execution_target_id, availability, version) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'active', $9, $10, $11, $12, 1)",
                    )
                    .bind(id.to_string())
                    .bind(&tenant)
                    .bind(&workspace)
                    .bind(&name)
                    .bind(&persona)
                    .bind(&standing_instructions)
                    .bind(json!(needs))
                    .bind(&memory_scope)
                    .bind(&template_id)
                    .bind(&model_route_preference)
                    .bind(&default_execution_target_id)
                    .bind(&availability)
                    .execute(&mut **tx)
                    .await?;
                    let teammate = load(tx, &tenant, &id).await?;
                    stage(tx, batch, &admin, &teammate, "agent.definition_created", json!({
                        "teammate_id": teammate.id.to_string(),
                        "workspace_id": teammate.workspace_id,
                        "name": teammate.name,
                        "template_id": teammate.template_id,
                        "default_capability_needs": teammate.default_capability_needs,
                        "version": teammate.version,
                        "definition_digest": teammate.digest().to_string(),
                    }))
                    .await?;
                    append_in(tx, &tenant, AuditDraft {
                        workspace_id: Some(teammate.workspace_id.clone()),
                        actor: actor_json(&admin.actor),
                        action: "CreateTeammate".to_string(),
                        target_ref: Some(format!("teammate:{}", teammate.id)),
                        decision: Some("applied".to_string()),
                        reason: Some(format!("{} v1{}", teammate.name, teammate.template_id.as_deref().map(|id| format!(" from {id}")).unwrap_or_default())),
                        correlation_id: Some(admin.correlation_id.to_string()),
                    })
                    .await?;
                    Ok(teammate)
                })
            })
            .await?;
        let thread = self.provision_thread(identity, &teammate).await?;
        Ok((teammate, thread))
    }

    /// The teammate's ACTIVE AgentThread, provisioning one when the workspace has none
    /// for it (one persistent thread per teammate per workspace).
    ///
    /// Safe to call from any number of messages at once: the teammate's row is committed before its thread exists, so the
    /// first messages can all arrive in that window. Exactly one creates the thread, they all get the same one, and none
    /// is refused because another activated it first.
    ///
    /// # Errors
    /// Returns the runtime's error.
    pub async fn provision_thread(
        &self,
        identity: &RuntimeIdentity,
        teammate: &Teammate,
    ) -> Result<AgentThread, TeammateError> {
        let agents = AgentStore::new(self.pool.clone(), identity.clone())?;
        let mut new = NewAgentThread::new(teammate.workspace_id.clone(), AgentKind::Teammate);
        new.definition_id = Some(teammate.id);
        if let Some(target) = &teammate.default_execution_target_id {
            new.execution_target_id =
                CanonicalId::parse_typed(target, Prefix::ExecutionTarget).ok();
        }
        // One thread per teammate however many messages ask for it at once, and an activation another caller got to first is
        // accepted: see `AgentStore::ensure_thread_for_definition`.
        let thread = agents.ensure_thread_for_definition(new).await?;
        Ok(agents.ensure_active(thread).await?)
    }

    /// Change a definition: version bumped, `agent.definition_updated` and the AuditEntry in
    /// the same transaction. An archived teammate cannot be changed.
    ///
    /// # Errors
    /// [`TeammateError::NotFound`], [`TeammateError::Schema`], [`TeammateError::Conflict`].
    pub async fn update(
        &self,
        admin: &TeammateAdmin,
        teammate_id: &CanonicalId,
        change: TeammateChange,
    ) -> Result<Teammate, TeammateError> {
        if let Some(needs) = &change.default_capability_needs {
            validate_needs(needs)?;
        }
        if let Some(scope) = &change.memory_scope {
            validate_memory_scope(scope)?;
        }
        if let Some(availability) = &change.availability {
            validate_availability(availability)?;
        }
        if let Some(name) = &change.name {
            if name.trim().is_empty() || name.len() > 120 {
                return Err(TeammateError::Schema(
                    "name must be 1..=120 characters".to_string(),
                ));
            }
        }
        if change
            .standing_instructions
            .as_ref()
            .is_some_and(|text| text.len() > 20_000)
            || change
                .persona
                .as_ref()
                .is_some_and(|text| text.len() > 4_000)
        {
            return Err(TeammateError::Schema(
                "persona is at most 4000 and standing instructions at most 20000 characters"
                    .to_string(),
            ));
        }
        let tenant = admin.tenant_id.clone();
        let tenant_scope = tenant.clone();
        let admin = admin.clone();
        let id = *teammate_id;
        self.commit(&tenant_scope, move |tx, batch| {
            Box::pin(async move {
                let before = load_for_update(tx, &tenant, &id).await?;
                if before.status != "active" {
                    return Err(TeammateError::Conflict(format!("teammate {id} is {}; an archived teammate cannot be changed", before.status)));
                }
                let mut after = before.clone();
                if let Some(name) = change.name { after.name = name.trim().to_string(); }
                if let Some(persona) = change.persona { after.persona = persona; }
                if let Some(text) = change.standing_instructions { after.standing_instructions = text; }
                if let Some(needs) = change.default_capability_needs { after.default_capability_needs = needs; }
                if let Some(scope) = change.memory_scope { after.memory_scope = scope; }
                if let Some(route) = change.model_route_preference { after.model_route_preference = route; }
                if let Some(target) = change.default_execution_target_id { after.default_execution_target_id = target; }
                if let Some(availability) = change.availability { after.availability = availability; }
                after.version = before.version + 1;
                sqlx::query(
                    "UPDATE teammates SET name = $3, persona = $4, standing_instructions = $5, default_capability_needs = $6, \
                     memory_scope = $7, model_route_preference = $8, default_execution_target_id = $9, availability = $10, \
                     version = $11, updated_at = now() WHERE tenant_id = $1 AND id = $2",
                )
                .bind(&tenant)
                .bind(id.to_string())
                .bind(&after.name)
                .bind(&after.persona)
                .bind(&after.standing_instructions)
                .bind(json!(after.default_capability_needs))
                .bind(&after.memory_scope)
                .bind(&after.model_route_preference)
                .bind(&after.default_execution_target_id)
                .bind(&after.availability)
                .bind(after.version)
                .execute(&mut **tx)
                .await?;
                let after = load(tx, &tenant, &id).await?;
                let changed: Vec<&str> = [
                    ("name", before.name != after.name),
                    ("persona", before.persona != after.persona),
                    ("standing_instructions", before.standing_instructions != after.standing_instructions),
                    ("default_capability_needs", before.default_capability_needs != after.default_capability_needs),
                    ("memory_scope", before.memory_scope != after.memory_scope),
                    ("model_route_preference", before.model_route_preference != after.model_route_preference),
                    ("default_execution_target_id", before.default_execution_target_id != after.default_execution_target_id),
                    ("availability", before.availability != after.availability),
                ]
                .into_iter()
                .filter_map(|(field, changed)| changed.then_some(field))
                .collect();
                stage(tx, batch, &admin, &after, "agent.definition_updated", json!({
                    "teammate_id": after.id.to_string(),
                    "workspace_id": after.workspace_id,
                    "from_version": before.version,
                    "version": after.version,
                    "changed": changed,
                    "default_capability_needs": after.default_capability_needs,
                    "definition_digest": after.digest().to_string(),
                }))
                .await?;
                append_in(tx, &tenant, AuditDraft {
                    workspace_id: Some(after.workspace_id.clone()),
                    actor: actor_json(&admin.actor),
                    action: "UpdateTeammate".to_string(),
                    target_ref: Some(format!("teammate:{}", after.id)),
                    decision: Some("applied".to_string()),
                    reason: Some(format!("v{} -> v{}: {}", before.version, after.version, changed.join(", "))),
                    correlation_id: Some(admin.correlation_id.to_string()),
                })
                .await?;
                Ok(after)
            })
        })
        .await
    }

    /// Archive a teammate: the definition becomes `archived` and its AgentThread(s) are
    /// suspended through RUN-002's transition (`teammate archived`). Every identity — the
    /// thread, its runs, messages, work and evidence — stays where it is.
    ///
    /// # Errors
    /// [`TeammateError::NotFound`], [`TeammateError::Conflict`] when already archived.
    pub async fn archive(
        &self,
        admin: &TeammateAdmin,
        identity: &RuntimeIdentity,
        teammate_id: &CanonicalId,
    ) -> Result<(Teammate, Vec<AgentThread>), TeammateError> {
        let tenant = admin.tenant_id.clone();
        let tenant_scope = tenant.clone();
        let admin_for_tx = admin.clone();
        let id = *teammate_id;
        let teammate = self
            .commit(&tenant_scope, move |tx, batch| {
                Box::pin(async move {
                    let before = load_for_update(tx, &tenant, &id).await?;
                    if before.status == "archived" {
                        return Err(TeammateError::Conflict(format!("teammate {id} is already archived")));
                    }
                    sqlx::query("UPDATE teammates SET status = 'archived', version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2")
                        .bind(&tenant)
                        .bind(id.to_string())
                        .execute(&mut **tx)
                        .await?;
                    let after = load(tx, &tenant, &id).await?;
                    stage(tx, batch, &admin_for_tx, &after, "agent.definition_archived", json!({
                        "teammate_id": after.id.to_string(),
                        "workspace_id": after.workspace_id,
                        "version": after.version,
                        "lineage_retained": true,
                    }))
                    .await?;
                    append_in(tx, &tenant, AuditDraft {
                        workspace_id: Some(after.workspace_id.clone()),
                        actor: actor_json(&admin_for_tx.actor),
                        action: "ArchiveTeammate".to_string(),
                        target_ref: Some(format!("teammate:{}", after.id)),
                        decision: Some("applied".to_string()),
                        reason: Some("threads suspended, lineage retained".to_string()),
                        correlation_id: Some(admin_for_tx.correlation_id.to_string()),
                    })
                    .await?;
                    Ok(after)
                })
            })
            .await?;
        // The thread transition is RUN-002's own generation-fenced mutation with its own
        // `agent.suspended` event; a thread that is already suspended or terminated is left
        // as it is (the definition is archived either way).
        let agents = AgentStore::new(self.pool.clone(), identity.clone())?;
        let mut suspended = Vec::new();
        for thread in agents.list_threads(&teammate.workspace_id).await? {
            if thread.definition_id.as_ref() != Some(&teammate.id) {
                continue;
            }
            if matches!(
                thread.status,
                AgentThreadStatus::Active | AgentThreadStatus::Provisioned
            ) {
                suspended.push(
                    agents
                        .suspend(&thread.id, thread.generation, "teammate archived")
                        .await?,
                );
            } else {
                suspended.push(thread);
            }
        }
        Ok((teammate, suspended))
    }

    async fn commit<T, F>(&self, tenant_id: &str, mutation: F) -> Result<T, TeammateError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxFuture<'a, T>,
    {
        let rejection: Arc<Mutex<Option<TeammateError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    match mutation(tx, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: TEAMMATE_OWNER,
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
                None => Err(TeammateError::Event(error)),
            },
        }
    }
}

fn actor_json(actor: &Actor) -> Value {
    serde_json::to_value(actor).unwrap_or(Value::Null)
}

async fn stage(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    admin: &TeammateAdmin,
    teammate: &Teammate,
    event_type: &str,
    payload: Value,
) -> Result<(), TeammateError> {
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND aggregate_type = 'teammate' AND aggregate_id = $2",
    )
    .bind(&admin.tenant_id)
    .bind(teammate.id.to_string())
    .fetch_one(&mut **tx)
    .await?;
    batch.emit(
        EventDraft::new(
            "teammate",
            teammate.id.to_string(),
            u64::try_from(existing).unwrap_or(0).saturating_add(1),
            EventType::parse(event_type)?,
            admin.correlation_id,
            admin.actor.clone(),
        )
        .with_workspace(teammate.workspace_id.clone())
        .with_payload(payload),
    );
    Ok(())
}

async fn load(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Teammate, TeammateError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM teammates WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(tenant_id)
    .bind(id.to_string())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| TeammateError::NotFound(id.to_string()))?;
    teammate_from_row(&row)
}

async fn load_for_update(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    id: &CanonicalId,
) -> Result<Teammate, TeammateError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM teammates WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(id.to_string())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| TeammateError::NotFound(id.to_string()))?;
    teammate_from_row(&row)
}

fn teammate_from_row(row: &PgRow) -> Result<Teammate, TeammateError> {
    let id: String = row.try_get("id")?;
    let needs: Value = row.try_get("default_capability_needs")?;
    Ok(Teammate {
        id: CanonicalId::parse_typed(&id, Prefix::Teammate)
            .map_err(|error| TeammateError::Schema(error.to_string()))?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        name: row.try_get("name")?,
        persona: row
            .try_get::<Option<String>, _>("persona")?
            .unwrap_or_default(),
        standing_instructions: row
            .try_get::<Option<String>, _>("standing_instructions")?
            .unwrap_or_default(),
        default_capability_needs: needs
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        memory_scope: row.try_get("memory_scope")?,
        status: row.try_get("status")?,
        template_id: row.try_get("template_id")?,
        model_route_preference: row.try_get("model_route_preference")?,
        default_execution_target_id: row.try_get("default_execution_target_id")?,
        availability: row.try_get("availability")?,
        version: row.try_get("version")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}
