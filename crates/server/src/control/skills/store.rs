//! The durable Skill registry (INT-009, DOMAIN.md §11.5).
//!
//! [`SkillStore`] owns the `skills` and `skill_versions` rows. Every mutation is one
//! transaction that also stages its `skill.*` RuntimeEvent, so the registry and the event
//! trail cannot disagree: a refused promotion writes neither.
//!
//! One invariant is the store's rather than the ladder's: a skill has at most one ACTIVE
//! version, and `skills.current_active_version_id` names exactly that version. Promoting
//! to ACTIVE is refused while another version is still active — `ACTIVE → DEPRECATED` is
//! the ladder's own edge, so the operator deprecates first — and leaving ACTIVE clears
//! the pointer. [`SkillStore::active_versions`] requires the state and the pointer to
//! agree, so a version reaches production context only when both say it may.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::event_type::EventType;
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::{promote, SkillControlError, SkillStatus, SKILLS_OWNER};

/// Boxed future returned by a skill mutation closure.
type BoxSkillFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SkillControlError>> + Send + 'a>>;

const SKILL_COLUMNS: &str = "id, tenant_id, scope, name, owner, current_active_version_id";
const VERSION_COLUMNS: &str = "id, tenant_id, skill_id, semver, manifest, provenance, status";

/// Who is making an administrative Skill change.
///
/// The control plane deliberately does not depend on the runtime's identity type: skills
/// are promoted by an administrator, not by a run, and the dependency direction in this
/// crate is runtime → control.
#[derive(Debug, Clone)]
pub struct SkillAdmin {
    /// Owning tenant; every statement is scoped to it and RLS enforces it.
    pub tenant_id: String,
    /// Who acted.
    pub actor: Actor,
    /// Everything caused by one external input shares this id.
    pub correlation_id: CorrelationId,
}

impl SkillAdmin {
    /// An administrative identity for a tenant.
    #[must_use]
    pub fn new(tenant_id: impl Into<String>, actor: Actor, correlation_id: CorrelationId) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            actor,
            correlation_id,
        }
    }
}

/// The `skills.scope` vocabulary (DOMAIN.md §11.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    /// Tenant-wide.
    Tenant,
    /// Scoped to one workspace.
    Workspace,
    /// Distributed as part of a capability pack.
    Pack,
}

impl SkillScope {
    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Workspace => "workspace",
            Self::Pack => "pack",
        }
    }

    /// Parse the durable spelling.
    ///
    /// # Errors
    /// Returns [`SkillControlError::Malformed`] for a value outside the vocabulary.
    pub fn parse(value: &str) -> Result<Self, SkillControlError> {
        match value.trim().to_lowercase().as_str() {
            "tenant" => Ok(Self::Tenant),
            "workspace" => Ok(Self::Workspace),
            "pack" => Ok(Self::Pack),
            other => Err(SkillControlError::Malformed(format!(
                "{other:?} is not a skill scope"
            ))),
        }
    }
}

/// Fields required to create a [`Skill`].
#[derive(Debug, Clone)]
pub struct NewSkill {
    /// Tenant, workspace or pack scope.
    pub scope: SkillScope,
    /// Unique name within the tenant.
    pub name: String,
    /// Accountable owner.
    pub owner: String,
}

/// Fields required to create a [`SkillVersion`].
#[derive(Debug, Clone)]
pub struct NewSkillVersion {
    /// Semantic version, unique within the skill.
    pub semver: String,
    /// The §11.5 manifest (`instructions`, `examples`, `tool_needs`, `capability_needs`, …).
    pub manifest: Value,
    /// Where the version came from.
    pub provenance: Option<String>,
}

/// A persisted `skills` row (DOMAIN.md §11.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Canonical `skl_…` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Tenant, workspace or pack scope.
    pub scope: SkillScope,
    /// Unique name within the tenant.
    pub name: String,
    /// Accountable owner.
    pub owner: String,
    /// The one version allowed to enter production context.
    pub current_active_version_id: Option<CanonicalId>,
}

/// A persisted `skill_versions` row (DOMAIN.md §11.5).
#[derive(Debug, Clone, PartialEq)]
pub struct SkillVersion {
    /// Canonical `sklv_…` identity.
    pub id: CanonicalId,
    /// Owning tenant.
    pub tenant_id: String,
    /// The skill this version belongs to.
    pub skill_id: CanonicalId,
    /// Semantic version, unique within the skill.
    pub semver: String,
    /// The §11.5 manifest.
    pub manifest: Value,
    /// Where the version came from.
    pub provenance: Option<String>,
    /// Position on the §11.5 ladder.
    pub status: SkillStatus,
}

impl SkillVersion {
    /// Whether this version may enter production context.
    #[must_use]
    pub const fn resolves(&self) -> bool {
        self.status.resolves()
    }
}

/// The outcome of a promotion.
#[derive(Debug, Clone, PartialEq)]
pub struct Promotion {
    /// The version in its new state.
    pub version: SkillVersion,
    /// The version that was ACTIVE before this promotion, if any.
    pub previous_active_version_id: Option<CanonicalId>,
}

/// The durable Skill registry.
#[derive(Debug, Clone)]
pub struct SkillStore {
    events: EventStore,
}

impl SkillStore {
    /// A store writing through `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            events: EventStore::new(pool),
        }
    }

    /// The PostgreSQL pool this store writes through.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        self.events.pool()
    }

    /// Create a skill with no active version.
    ///
    /// # Errors
    /// Returns the control-plane refusal when the row cannot be written.
    pub async fn create_skill(
        &self,
        admin: &SkillAdmin,
        new: NewSkill,
    ) -> Result<Skill, SkillControlError> {
        let tenant_id = admin.tenant_id.clone();
        let admin = admin.clone();
        self.commit(&tenant_id, move |tx, batch| {
            Box::pin(async move {
                let id = CanonicalId::generate(Prefix::Skill, &mut UlidGenerator::new());
                sqlx::query(
                    "INSERT INTO skills (id, tenant_id, scope, name, owner) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(id.to_string())
                .bind(&admin.tenant_id)
                .bind(new.scope.as_str())
                .bind(&new.name)
                .bind(&new.owner)
                .execute(&mut **tx)
                .await?;
                let skill = Skill {
                    id,
                    tenant_id: admin.tenant_id.clone(),
                    scope: new.scope,
                    name: new.name,
                    owner: new.owner,
                    current_active_version_id: None,
                };
                stage(
                    &admin,
                    tx,
                    batch,
                    &skill,
                    1,
                    "skill.created",
                    json!({
                        "skill_id": skill.id.to_string(),
                        "scope": skill.scope.as_str(),
                        "name": skill.name,
                        "owner": skill.owner,
                    }),
                )
                .await?;
                Ok(skill)
            })
        })
        .await
    }

    /// Add a `DRAFT` version to an existing skill.
    ///
    /// # Errors
    /// Returns [`SkillControlError::SkillNotFound`] when the skill is not in this tenant.
    pub async fn create_version(
        &self,
        admin: &SkillAdmin,
        skill_id: &CanonicalId,
        new: NewSkillVersion,
    ) -> Result<SkillVersion, SkillControlError> {
        let tenant_id = admin.tenant_id.clone();
        let admin = admin.clone();
        let skill_id = *skill_id;
        self.commit(&tenant_id, move |tx, batch| {
            Box::pin(async move {
                let skill = load_skill(tx, &admin.tenant_id, &skill_id, false).await?;
                let id = CanonicalId::generate(Prefix::SkillVersion, &mut UlidGenerator::new());
                sqlx::query(
                    "INSERT INTO skill_versions \
                     (id, tenant_id, skill_id, semver, manifest, provenance, status) \
                     VALUES ($1, $2, $3, $4, $5, $6, 'draft')",
                )
                .bind(id.to_string())
                .bind(&admin.tenant_id)
                .bind(skill.id.to_string())
                .bind(&new.semver)
                .bind(&new.manifest)
                .bind(new.provenance.as_deref())
                .execute(&mut **tx)
                .await?;
                let version = SkillVersion {
                    id,
                    tenant_id: admin.tenant_id.clone(),
                    skill_id: skill.id,
                    semver: new.semver,
                    manifest: new.manifest,
                    provenance: new.provenance,
                    status: SkillStatus::Draft,
                };
                stage(
                    &admin,
                    tx,
                    batch,
                    &skill,
                    1,
                    "skill.version_created",
                    json!({
                        "skill_id": skill.id.to_string(),
                        "skill_version_id": version.id.to_string(),
                        "semver": version.semver,
                        "status": version.status.as_str(),
                        "provenance": version.provenance,
                    }),
                )
                .await?;
                Ok(version)
            })
        })
        .await
    }

    /// Move a version along the §11.5 ladder.
    ///
    /// Promoting to ACTIVE sets `skills.current_active_version_id`; leaving ACTIVE clears
    /// it. The ladder check, the one-active-version rule and the pointer update all happen
    /// inside the transaction that stages the event, so a refusal leaves the registry
    /// exactly as it was.
    ///
    /// # Errors
    /// Returns [`SkillControlError::State`] for an edge the ladder does not have,
    /// [`SkillControlError::VersionNotFound`] for an unknown version and
    /// [`SkillControlError::AnotherVersionIsActive`] when a different version is active.
    pub async fn promote(
        &self,
        admin: &SkillAdmin,
        version_id: &CanonicalId,
        to: SkillStatus,
    ) -> Result<Promotion, SkillControlError> {
        let tenant_id = admin.tenant_id.clone();
        let admin = admin.clone();
        let version_id = *version_id;
        self.commit(&tenant_id, move |tx, batch| {
            Box::pin(async move {
                let mut version = load_version(tx, &admin.tenant_id, &version_id).await?;
                let skill = load_skill(tx, &admin.tenant_id, &version.skill_id, true).await?;
                let from = version.status;
                let next = promote(from, to)?;
                let previous_active = skill.current_active_version_id;
                if next == SkillStatus::Active {
                    if let Some(active) = &previous_active {
                        if active != &version.id {
                            return Err(SkillControlError::AnotherVersionIsActive {
                                skill_id: skill.id.to_string(),
                                active_version_id: active.to_string(),
                                version_id: version.id.to_string(),
                            });
                        }
                    }
                    sqlx::query(
                        "UPDATE skills SET current_active_version_id = $3, updated_at = now() \
                         WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&admin.tenant_id)
                    .bind(skill.id.to_string())
                    .bind(version.id.to_string())
                    .execute(&mut **tx)
                    .await?;
                } else if from == SkillStatus::Active {
                    sqlx::query(
                        "UPDATE skills SET current_active_version_id = NULL, \
                         updated_at = now() WHERE tenant_id = $1 AND id = $2 \
                         AND current_active_version_id = $3",
                    )
                    .bind(&admin.tenant_id)
                    .bind(skill.id.to_string())
                    .bind(version.id.to_string())
                    .execute(&mut **tx)
                    .await?;
                }
                sqlx::query(
                    "UPDATE skill_versions SET status = $3, updated_at = now() \
                     WHERE tenant_id = $1 AND id = $2",
                )
                .bind(&admin.tenant_id)
                .bind(version.id.to_string())
                .bind(next.as_str())
                .execute(&mut **tx)
                .await?;
                version.status = next;
                stage(&admin, tx, batch, &skill, 1, "skill.version_promoted", json!({
                    "skill_id": skill.id.to_string(),
                    "skill_version_id": version.id.to_string(),
                    "from": from.as_str(),
                    "to": next.as_str(),
                    "previous_active_version_id": previous_active.as_ref().map(ToString::to_string),
                    "resolves": next.resolves(),
                }))
                .await?;
                Ok(Promotion {
                    version,
                    previous_active_version_id: previous_active,
                })
            })
        })
        .await
    }

    /// Every skill with its active version, ordered by name.
    ///
    /// This is the view the resolver consumes. A version appears only when its own state
    /// is ACTIVE *and* its skill's pointer names it, so a row left inconsistent by
    /// anything but this store cannot enter production context.
    ///
    /// # Errors
    /// Returns the control-plane refusal when the view cannot be read.
    pub async fn active_versions(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<(Skill, SkillVersion)>, SkillControlError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let rows = sqlx::query(
            "SELECT s.id AS skill_id, s.tenant_id AS skill_tenant_id, s.scope, s.name, \
             s.owner, s.current_active_version_id, v.id AS version_id, v.semver, \
             v.manifest, v.provenance, v.status \
             FROM skills s JOIN skill_versions v ON v.id = s.current_active_version_id \
             WHERE s.tenant_id = $1 AND v.status = 'active' \
             ORDER BY s.name, s.id, v.semver, v.id",
        )
        .bind(tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(map_active_pair).collect()
    }

    /// Run one mutation that must commit a `skill.*` event with it.
    ///
    /// `EventStore::commit_mutation_tx` fixes its closure's error type to [`EventError`],
    /// so the typed refusal travels out of band the same way it does in the runtime
    /// stores: the transaction is rolled back with `MutationRejected` and the original
    /// error is returned to the caller.
    async fn commit<T, F>(&self, tenant_id: &str, mutation: F) -> Result<T, SkillControlError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxSkillFuture<'a, T>,
    {
        let rejection: Arc<Mutex<Option<SkillControlError>>> = Arc::new(Mutex::new(None));
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
                                owner: SKILLS_OWNER,
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
                None => Err(SkillControlError::Event(error)),
            },
        }
    }
}

/// Stage one `skill.*` event at the aggregate's next version.
async fn stage(
    admin: &SkillAdmin,
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    skill: &Skill,
    version: u64,
    event_type: &str,
    payload: Value,
) -> Result<(), SkillControlError> {
    let aggregate_id = skill.id.to_string();
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 \
         AND aggregate_type = 'skill' AND aggregate_id = $2",
    )
    .bind(&admin.tenant_id)
    .bind(&aggregate_id)
    .fetch_one(&mut **tx)
    .await?;
    let aggregate_version = u64::try_from(existing).unwrap_or(0).saturating_add(version);
    batch.emit(
        EventDraft::new(
            "skill",
            &aggregate_id,
            aggregate_version,
            EventType::parse(event_type)?,
            admin.correlation_id,
            admin.actor.clone(),
        )
        .with_payload(payload),
    );
    Ok(())
}

/// Read one skill, optionally taking the row lock a promotion needs.
async fn load_skill(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    skill_id: &CanonicalId,
    for_update: bool,
) -> Result<Skill, SkillControlError> {
    let sql = if for_update {
        format!("SELECT {SKILL_COLUMNS} FROM skills WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
    } else {
        format!("SELECT {SKILL_COLUMNS} FROM skills WHERE tenant_id = $1 AND id = $2")
    };
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(skill_id.to_string())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| SkillControlError::SkillNotFound {
            skill_id: skill_id.to_string(),
        })?;
    map_skill(&row)
}

/// Read one skill version, taking the row lock a promotion needs.
async fn load_version(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    version_id: &CanonicalId,
) -> Result<SkillVersion, SkillControlError> {
    let row = sqlx::query(&format!(
        "SELECT {VERSION_COLUMNS} FROM skill_versions \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(version_id.to_string())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| SkillControlError::VersionNotFound {
        version_id: version_id.to_string(),
    })?;
    map_version(&row)
}

fn map_skill(row: &PgRow) -> Result<Skill, SkillControlError> {
    Ok(Skill {
        id: canonical(row, "id", Prefix::Skill)?,
        tenant_id: row.try_get("tenant_id")?,
        scope: SkillScope::parse(&row.try_get::<String, _>("scope")?)?,
        name: row.try_get("name")?,
        owner: row.try_get("owner")?,
        current_active_version_id: match row
            .try_get::<Option<String>, _>("current_active_version_id")?
        {
            Some(value) => Some(CanonicalId::parse_typed(&value, Prefix::SkillVersion)?),
            None => None,
        },
    })
}

fn map_version(row: &PgRow) -> Result<SkillVersion, SkillControlError> {
    Ok(SkillVersion {
        id: canonical(row, "id", Prefix::SkillVersion)?,
        tenant_id: row.try_get("tenant_id")?,
        skill_id: canonical(row, "skill_id", Prefix::Skill)?,
        semver: row.try_get("semver")?,
        manifest: row.try_get("manifest")?,
        provenance: row.try_get("provenance")?,
        status: SkillStatus::parse(&row.try_get::<String, _>("status")?)
            .map_err(SkillControlError::from)?,
    })
}

fn map_active_pair(row: &PgRow) -> Result<(Skill, SkillVersion), SkillControlError> {
    Ok((
        Skill {
            id: canonical(row, "skill_id", Prefix::Skill)?,
            tenant_id: row.try_get("skill_tenant_id")?,
            scope: SkillScope::parse(&row.try_get::<String, _>("scope")?)?,
            name: row.try_get("name")?,
            owner: row.try_get("owner")?,
            current_active_version_id: match row
                .try_get::<Option<String>, _>("current_active_version_id")?
            {
                Some(value) => Some(CanonicalId::parse_typed(&value, Prefix::SkillVersion)?),
                None => None,
            },
        },
        SkillVersion {
            id: canonical(row, "version_id", Prefix::SkillVersion)?,
            tenant_id: row.try_get("skill_tenant_id")?,
            skill_id: canonical(row, "skill_id", Prefix::Skill)?,
            semver: row.try_get("semver")?,
            manifest: row.try_get("manifest")?,
            provenance: row.try_get("provenance")?,
            status: SkillStatus::parse(&row.try_get::<String, _>("status")?)
                .map_err(SkillControlError::from)?,
        },
    ))
}

fn canonical(row: &PgRow, column: &str, prefix: Prefix) -> Result<CanonicalId, SkillControlError> {
    let value: String = row.try_get(column)?;
    CanonicalId::parse_typed(&value, prefix).map_err(SkillControlError::from)
}
