//! The pack store: revisions, activation, retention, rollback and install reports (ADP-008, DOMAIN.md 11.6).
//!
//! Everything here is one tenant's rows under row-level security, in the tables migration 0001 made for packs
//! (`capability_packs`, `capability_pack_versions`) and the one migration 0029 added (`pack_install_reports`). A
//! pack row's `current_published_version_id` is the **active** revision. Activation is one transaction that
//! moves that pointer, retires the revision it replaced (kept, with its content, for rollback), evicts what is
//! past the retention bound and finishes the install report: a crash before it leaves the previous revision
//! active, and the report `running` for the next sweep to close.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Duration, Utc};
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::event_type::EventType;
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::config::PackConfig;
use super::content::PackContent;
use super::report::{sanitize_all, Diagnostic, InstallReport, Outcome, Phase, Severity, State};

/// Repository path of this module's canonical owner.
pub const PACKS_OWNER: &str = "crates/server/src/control/packs";

/// A pack store refusal.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// A database failure.
    #[error("pack store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The event store refused the transaction that carried the mutation.
    #[error(transparent)]
    Event(#[from] EventError),
    /// The active revision is not the one the caller expected.
    #[error("pack {pack}: expected {expected:?} to be active, found {current:?}")]
    Conflict {
        /// The pack.
        pack: String,
        /// What the caller believed was active.
        expected: Option<String>,
        /// What is.
        current: Option<String>,
    },
    /// There is no retained previous revision to roll back to.
    #[error("pack {pack} has no retained previous revision to roll back to")]
    NoPrevious {
        /// The pack.
        pack: String,
    },
    /// No such pack.
    #[error("pack {pack} is not installed")]
    NotFound {
        /// The name asked for.
        pack: String,
    },
    /// The same request is still installing.
    #[error("install {request_id} is still running")]
    InFlight {
        /// The command id.
        request_id: String,
    },
    /// Content the store cannot keep.
    #[error("{0}")]
    Malformed(String),
}

impl PackError {
    /// The DOMAIN.md section 15 code this refusal maps to.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Database(_) | Self::Event(_) => "INTERNAL",
            Self::Conflict { .. } => "CONFLICT_REVISION",
            Self::NoPrevious { .. } | Self::InFlight { .. } => "CONFLICT_STATE",
            Self::NotFound { .. } => "NOT_FOUND",
            Self::Malformed(_) => "VALIDATION_SCHEMA",
        }
    }
}

/// What a `begin` found.
#[derive(Debug)]
pub enum Begun {
    /// A new report is running.
    Started(InstallReport),
    /// This request id was already used; here is what it left.
    Existing(InstallReport),
}

/// A revision of a pack as the listing shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct RevisionInfo {
    /// `pckv_` row.
    pub version_id: String,
    /// The content-hash revision.
    pub revision: Option<String>,
    /// The manifest's semver.
    pub semver: String,
    /// `published` (active), `deprecated` (retained), `withdrawn` (history only).
    pub status: String,
    /// Whether the content is still held (so the revision can be activated again).
    pub has_content: bool,
    /// When it was last activated.
    pub activated_at: Option<DateTime<Utc>>,
    /// When it was replaced.
    pub retired_at: Option<DateTime<Utc>>,
}

impl RevisionInfo {
    fn to_json(&self) -> Value {
        json!({
            "version_id": self.version_id,
            "revision": self.revision,
            "semver": self.semver,
            "status": self.status,
            "has_content": self.has_content,
            "activated_at": self.activated_at,
            "retired_at": self.retired_at,
        })
    }
}

/// A pack with its revisions.
#[derive(Debug, Clone, PartialEq)]
pub struct PackSummary {
    /// `pck_` row.
    pub pack_id: String,
    /// The pack's name.
    pub name: String,
    /// The active revision, if any.
    pub active: Option<RevisionInfo>,
    /// Every other revision, newest first.
    pub revisions: Vec<RevisionInfo>,
}

impl PackSummary {
    /// The API shape.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "pack_id": self.pack_id,
            "name": self.name,
            "active": self.active.as_ref().map(RevisionInfo::to_json),
            "revisions": self.revisions.iter().map(RevisionInfo::to_json).collect::<Vec<_>>(),
        })
    }
}

/// What an activation did.
#[derive(Debug, Clone)]
pub struct Activated {
    /// `pck_` row.
    pub pack_id: String,
    /// `pckv_` row that is now active.
    pub version_id: String,
    /// The active revision.
    pub revision: String,
    /// The revision it replaced.
    pub previous_revision: Option<String>,
    /// Revisions that fell out of retention.
    pub evicted: Vec<String>,
    /// The revision was already active: nothing moved.
    pub unchanged: bool,
    /// The finished report.
    pub report: InstallReport,
}

/// What a rollback did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolledBack {
    /// `pck_` row.
    pub pack_id: String,
    /// The pack's name.
    pub name: String,
    /// The revision that was active.
    pub from_revision: Option<String>,
    /// The revision that is.
    pub to_revision: String,
    /// `pckv_` row that is now active.
    pub version_id: String,
}

/// Everything an activation needs.
#[derive(Debug)]
pub struct Activation<'a> {
    /// The command id.
    pub request_id: &'a str,
    /// The content to activate.
    pub content: &'a PackContent,
    /// Its revision, as the server computed it.
    pub revision: &'a str,
    /// Findings that did not stop the install.
    pub warnings: &'a [Diagnostic],
    /// What the plane says it resolves to (kept in the event).
    pub resolution: &'a Value,
    /// The revision the caller believed was active: `Some(None)` is "nothing".
    pub expected_active: Option<Option<&'a str>>,
    /// Who installs.
    pub actor: &'a Actor,
    /// Correlation of the command.
    pub correlation_id: CorrelationId,
}

/// The pack store.
#[derive(Debug, Clone)]
pub struct PackStore {
    events: EventStore,
    config: Arc<PackConfig>,
}

/// The future a pack mutation returns.
type BoxPackFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PackError>> + Send + 'a>>;

fn id_of(prefix: Prefix) -> String {
    CanonicalId::generate(prefix, &mut UlidGenerator::new()).to_string()
}

fn event_type(name: &str) -> Result<EventType, EventError> {
    EventType::parse(name)
}

async fn next_version(
    tx: &mut Transaction<'static, Postgres>,
    batch: &EventBatch,
    tenant_id: &str,
    pack_id: &str,
) -> Result<u64, sqlx::Error> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND aggregate_type = 'pack' AND aggregate_id = $2",
    )
    .bind(tenant_id)
    .bind(pack_id)
    .fetch_one(&mut **tx)
    .await?;
    let staged = batch.staged_version("pack", pack_id).unwrap_or(0);
    Ok(u64::try_from(committed).unwrap_or(0).max(staged) + 1)
}

fn report_of(row: &PgRow) -> Result<InstallReport, PackError> {
    let diagnostics: Value = row.try_get("diagnostics")?;
    let state: String = row.try_get("state")?;
    let phase: String = row.try_get("phase")?;
    let outcome: Option<String> = row.try_get("outcome")?;
    Ok(InstallReport {
        request_id: row.try_get("request_id")?,
        pack_name: row.try_get("pack_name")?,
        pack_id: row.try_get("pack_id")?,
        revision: row.try_get("revision")?,
        previous_revision: row.try_get("previous_revision")?,
        state: State::parse(&state)
            .ok_or_else(|| PackError::Malformed(format!("report state {state:?}")))?,
        phase: Phase::parse(&phase)
            .ok_or_else(|| PackError::Malformed(format!("report phase {phase:?}")))?,
        outcome: outcome.as_deref().and_then(Outcome::parse),
        diagnostics: diagnostics
            .as_array()
            .map(|items| items.iter().filter_map(Diagnostic::from_json).collect())
            .unwrap_or_default(),
        diagnostics_omitted: row.try_get::<i32, _>("diagnostics_omitted")?.into(),
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        expires_at: row.try_get("expires_at")?,
    })
}

const REPORT_COLUMNS: &str =
    "request_id, pack_name, pack_id, revision, previous_revision, state, phase, outcome, \
     diagnostics, diagnostics_omitted, started_at, finished_at, expires_at";

fn revision_of_row(row: &PgRow) -> Result<RevisionInfo, PackError> {
    let manifest: Option<Value> = row.try_get("manifest")?;
    Ok(RevisionInfo {
        version_id: row.try_get("id")?,
        revision: row.try_get("revision")?,
        semver: row.try_get("semver")?,
        status: row.try_get("status")?,
        has_content: manifest.is_some(),
        activated_at: row.try_get("activated_at")?,
        retired_at: row.try_get("retired_at")?,
    })
}

impl PackStore {
    /// A store over `pool` with the given policy.
    #[must_use]
    pub fn new(pool: PgPool, config: Arc<PackConfig>) -> Self {
        Self {
            events: EventStore::new(pool),
            config,
        }
    }

    /// The policy in force.
    #[must_use]
    pub fn config(&self) -> &PackConfig {
        &self.config
    }

    /// The pool.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        self.events.pool()
    }

    /// Commit one mutation with its RuntimeEvent. A refusal rolls the transaction back and comes out as itself,
    /// not as the event store's wrapper.
    async fn commit<T, F>(&self, tenant_id: &str, mutation: F) -> Result<T, PackError>
    where
        T: Send + 'static,
        F: Send
            + 'static
            + for<'a> FnOnce(
                &'a mut Transaction<'static, Postgres>,
                &'a mut EventBatch,
            ) -> BoxPackFuture<'a, T>,
    {
        let slot: Arc<Mutex<Option<PackError>>> = Arc::new(Mutex::new(None));
        let carried = Arc::clone(&slot);
        let result = self
            .events
            .commit_mutation_tx(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    match mutation(tx, batch).await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            let message = error.to_string();
                            *carried.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                            Err(EventError::MutationRejected {
                                owner: PACKS_OWNER,
                                message,
                            })
                        }
                    }
                })
            })
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(error) => match slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                Some(refusal) => Err(refusal),
                None => Err(PackError::Event(error)),
            },
        }
    }

    /// Purge what expired and close what was left running, in `tx`: the repository's lazy sweep.
    async fn sweep(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
    ) -> Result<(), PackError> {
        sqlx::query(
            "DELETE FROM pack_install_reports WHERE tenant_id = $1 AND expires_at <= now()",
        )
        .bind(tenant_id)
        .execute(&mut **tx)
        .await?;
        let interrupted = Diagnostic::error(
            Phase::Activate,
            "pack.install.interrupted",
            "the server stopped before this install finished; the previous revision stayed active",
        )
        .to_json();
        sqlx::query(
            "UPDATE pack_install_reports r SET state = 'failed', finished_at = now(), \
             outcome = CASE WHEN EXISTS (SELECT 1 FROM capability_packs p \
                 WHERE p.tenant_id = r.tenant_id AND p.name = r.pack_name \
                   AND p.current_published_version_id IS NOT NULL) \
               THEN 'retained_previous' ELSE 'failed' END, \
             diagnostics = r.diagnostics || jsonb_build_array(jsonb_set($3::jsonb, '{phase}', to_jsonb(r.phase))) \
             WHERE r.tenant_id = $1 AND r.state = 'running' \
               AND r.started_at < now() - make_interval(secs => $2::double precision)",
        )
        .bind(tenant_id)
        .bind(self.config.install.running_grace_seconds as f64)
        .bind(interrupted)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn active_revision(
        tx: &mut Transaction<'static, Postgres>,
        tenant_id: &str,
        name: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT v.revision FROM capability_packs p \
             JOIN capability_pack_versions v ON v.id = p.current_published_version_id \
             WHERE p.tenant_id = $1 AND p.name = $2",
        )
        .bind(tenant_id)
        .bind(name)
        .fetch_optional(&mut **tx)
        .await
        .map(Option::flatten)
    }

    /// Start a report for `request_id`, or hand back the one that already exists.
    ///
    /// # Errors
    /// A database failure.
    pub async fn begin(
        &self,
        tenant_id: &str,
        request_id: &str,
        pack_hint: &str,
    ) -> Result<Begun, PackError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        self.sweep(&mut tx, tenant_id).await?;
        let previous = Self::active_revision(&mut tx, tenant_id, pack_hint).await?;
        let expires = Utc::now() + Duration::days(self.config.reports.retention_days);
        let inserted = sqlx::query(&format!(
            "INSERT INTO pack_install_reports (tenant_id, request_id, pack_name, previous_revision, state, phase, expires_at) \
             VALUES ($1, $2, $3, $4, 'running', 'fetch', $5) ON CONFLICT (tenant_id, request_id) DO NOTHING \
             RETURNING {REPORT_COLUMNS}"
        ))
        .bind(tenant_id)
        .bind(request_id)
        .bind(pack_hint)
        .bind(&previous)
        .bind(expires)
        .fetch_optional(&mut *tx)
        .await?;
        let begun = match inserted {
            Some(row) => Begun::Started(report_of(&row)?),
            None => {
                let row = sqlx::query(&format!(
                    "SELECT {REPORT_COLUMNS} FROM pack_install_reports WHERE tenant_id = $1 AND request_id = $2"
                ))
                .bind(tenant_id)
                .bind(request_id)
                .fetch_one(&mut *tx)
                .await?;
                Begun::Existing(report_of(&row)?)
            }
        };
        tx.commit().await?;
        Ok(begun)
    }

    /// Record that the install reached `phase`, with what is known by then.
    ///
    /// # Errors
    /// A database failure.
    pub async fn set_phase(
        &self,
        tenant_id: &str,
        request_id: &str,
        phase: Phase,
        pack_name: Option<&str>,
        revision: Option<&str>,
    ) -> Result<(), PackError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        sqlx::query(
            "UPDATE pack_install_reports SET phase = $3, pack_name = COALESCE($4, pack_name), \
             revision = COALESCE($5, revision) \
             WHERE tenant_id = $1 AND request_id = $2 AND state = 'running'",
        )
        .bind(tenant_id)
        .bind(request_id)
        .bind(phase.as_str())
        .bind(pack_name)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Finish the report as failed at `phase`, with the findings, and stage `pack.install_failed`. The pack's
    /// active revision is not touched: the outcome says whether one is still there.
    ///
    /// # Errors
    /// A database or event failure.
    pub async fn fail(
        &self,
        tenant_id: &str,
        request_id: &str,
        phase: Phase,
        diagnostics: &[Diagnostic],
        actor: &Actor,
        correlation_id: CorrelationId,
    ) -> Result<InstallReport, PackError> {
        let (kept, omitted) = sanitize_all(diagnostics, &self.config.reports);
        let stored = Value::Array(kept.iter().map(Diagnostic::to_json).collect());
        let tenant = tenant_id.to_string();
        let request = request_id.to_string();
        let actor = actor.clone();
        let rows = self
            .commit(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let current = sqlx::query(
                        "SELECT pack_name, pack_id, revision FROM pack_install_reports \
                         WHERE tenant_id = $1 AND request_id = $2 FOR UPDATE",
                    )
                    .bind(&tenant)
                    .bind(&request)
                    .fetch_one(&mut **tx)
                    .await?;
                    let name: String = current.try_get("pack_name")?;
                    let revision: Option<String> = current.try_get("revision")?;
                    let active = PackStore::active_revision(tx, &tenant, &name).await?;
                    let pack_id: Option<String> = sqlx::query_scalar(
                        "SELECT id FROM capability_packs WHERE tenant_id = $1 AND name = $2",
                    )
                    .bind(&tenant)
                    .bind(&name)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let outcome = if active.is_some() {
                        Outcome::RetainedPrevious
                    } else {
                        Outcome::Failed
                    };
                    let row = sqlx::query(&format!(
                        "UPDATE pack_install_reports SET state = 'failed', phase = $3, outcome = $4, \
                         diagnostics = $5, diagnostics_omitted = $6, pack_id = $7, finished_at = now() \
                         WHERE tenant_id = $1 AND request_id = $2 AND state = 'running' \
                         RETURNING {REPORT_COLUMNS}"
                    ))
                    .bind(&tenant)
                    .bind(&request)
                    .bind(phase.as_str())
                    .bind(outcome.as_str())
                    .bind(&stored)
                    .bind(i32::try_from(omitted).unwrap_or(i32::MAX))
                    .bind(&pack_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                    // The report was already finished (a sweep closed it, or a replay): nothing moves, but a commit
                    // needs its event, so the failure is recorded once more against the request.
                    let aggregate = pack_id.clone().unwrap_or_else(|| request.clone());
                    let version = if pack_id.is_some() {
                        next_version(tx, batch, &tenant, &aggregate).await?
                    } else {
                        1
                    };
                    batch.emit(
                        EventDraft::new(
                            "pack",
                            aggregate,
                            version,
                            event_type("pack.install_failed")?,
                            correlation_id,
                            actor,
                        )
                        .with_payload(json!({
                            "request_id": request,
                            "pack": name,
                            "revision": revision,
                            "phase": phase.as_str(),
                            "outcome": outcome.as_str(),
                            "rules": kept.iter().map(|d| d.rule.clone()).collect::<Vec<_>>(),
                        })),
                    );
                    Ok(row)
                })
            })
            .await?;
        match rows {
            Some(row) => report_of(&row),
            None => self
                .report(tenant_id, request_id)
                .await?
                .ok_or_else(|| PackError::Malformed("the report vanished".to_string())),
        }
    }

    /// Make `content` the active revision of its pack, in one transaction, and finish the report.
    ///
    /// # Errors
    /// [`PackError::Conflict`] when `expected_active` is not what is active; a database or event failure
    /// (the transaction rolls back and the previous revision stays active).
    pub async fn activate(
        &self,
        tenant_id: &str,
        activation: Activation<'_>,
    ) -> Result<Activated, PackError> {
        let name = activation
            .content
            .name()
            .ok_or_else(|| PackError::Malformed("the manifest has no name".to_string()))?
            .to_string();
        if super::content::report_name(&name) != name {
            return Err(PackError::Malformed(
                "a pack's name is lowercase letters, digits, - or _".to_string(),
            ));
        }
        let semver = activation
            .content
            .semver()
            .ok_or_else(|| PackError::Malformed("the manifest has no semver".to_string()))?
            .to_string();
        let manifest = activation.content.manifest.clone();
        let files = serde_json::to_value(&activation.content.files)
            .map_err(|error| PackError::Malformed(error.to_string()))?;
        let contents = manifest
            .get("contents")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let evidence = manifest
            .get("evidence_requirements")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let provenance = manifest.get("provenance").map(Value::to_string);
        let (kept, omitted) = sanitize_all(activation.warnings, &self.config.reports);
        let stored = Value::Array(kept.iter().map(Diagnostic::to_json).collect());
        let retain =
            i64::try_from(self.config.install.retain_previous_revisions).unwrap_or(i64::MAX);
        let tenant = tenant_id.to_string();
        let request = activation.request_id.to_string();
        let revision = activation.revision.to_string();
        let expected: Option<Option<String>> = activation
            .expected_active
            .map(|expected| expected.map(str::to_string));
        let resolution = activation.resolution.clone();
        let actor = activation.actor.clone();
        let correlation_id = activation.correlation_id;
        let outcome = self
            .commit(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let fresh_pack = id_of(Prefix::CapabilityPack);
                    sqlx::query(
                        "INSERT INTO capability_packs (id, tenant_id, name) VALUES ($1, $2, $3) \
                         ON CONFLICT (tenant_id, name) DO NOTHING",
                    )
                    .bind(&fresh_pack)
                    .bind(&tenant)
                    .bind(&name)
                    .execute(&mut **tx)
                    .await?;
                    let pack = sqlx::query(
                        "SELECT id, current_published_version_id FROM capability_packs \
                         WHERE tenant_id = $1 AND name = $2 FOR UPDATE",
                    )
                    .bind(&tenant)
                    .bind(&name)
                    .fetch_one(&mut **tx)
                    .await?;
                    let pack_id: String = pack.try_get("id")?;
                    let current_version: Option<String> =
                        pack.try_get("current_published_version_id")?;
                    let current_revision: Option<String> = match &current_version {
                        Some(id) => {
                            sqlx::query_scalar(
                                "SELECT revision FROM capability_pack_versions WHERE id = $1",
                            )
                            .bind(id)
                            .fetch_optional(&mut **tx)
                            .await?
                            .flatten()
                        }
                        None => None,
                    };
                    if let Some(expected) = &expected {
                        if expected != &current_revision {
                            return Err(PackError::Conflict {
                                pack: name.clone(),
                                expected: expected.clone(),
                                current: current_revision,
                            });
                        }
                    }
                    let existing: Option<(String, String)> = sqlx::query_as(
                        "SELECT id, status FROM capability_pack_versions \
                         WHERE pack_id = $1 AND revision = $2",
                    )
                    .bind(&pack_id)
                    .bind(&revision)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let unchanged = current_revision.as_deref() == Some(revision.as_str());
                    let mut evicted: Vec<String> = Vec::new();
                    let version_id = if unchanged {
                        current_version.clone().unwrap_or_default()
                    } else {
                        if let Some(current) = &current_version {
                            sqlx::query(
                                "UPDATE capability_pack_versions SET status = 'deprecated', retired_at = now() \
                                 WHERE id = $1",
                            )
                            .bind(current)
                            .execute(&mut **tx)
                            .await?;
                        }
                        let version_id = match existing {
                            Some((id, _)) => {
                                sqlx::query(
                                    "UPDATE capability_pack_versions SET status = 'published', semver = $2, \
                                     contents = $3, provenance = $4, evidence_requirements = $5, manifest = $6, \
                                     files = $7, activated_at = now(), retired_at = NULL WHERE id = $1",
                                )
                                .bind(&id)
                                .bind(&semver)
                                .bind(&contents)
                                .bind(&provenance)
                                .bind(&evidence)
                                .bind(&manifest)
                                .bind(&files)
                                .execute(&mut **tx)
                                .await?;
                                id
                            }
                            None => {
                                let id = id_of(Prefix::CapabilityPackVersion);
                                sqlx::query(
                                    "INSERT INTO capability_pack_versions (id, tenant_id, pack_id, semver, \
                                     contents, provenance, evidence_requirements, status, revision, manifest, \
                                     files, activated_at) \
                                     VALUES ($1, $2, $3, $4, $5, $6, $7, 'published', $8, $9, $10, now())",
                                )
                                .bind(&id)
                                .bind(&tenant)
                                .bind(&pack_id)
                                .bind(&semver)
                                .bind(&contents)
                                .bind(&provenance)
                                .bind(&evidence)
                                .bind(&revision)
                                .bind(&manifest)
                                .bind(&files)
                                .execute(&mut **tx)
                                .await?;
                                id
                            }
                        };
                        sqlx::query(
                            "UPDATE capability_packs SET current_published_version_id = $2 WHERE id = $1",
                        )
                        .bind(&pack_id)
                        .bind(&version_id)
                        .execute(&mut **tx)
                        .await?;
                        // Retention: the newest `retain` retired revisions keep their content for rollback; older
                        // ones keep their row (history) and lose it.
                        let beyond: Vec<(String, Option<String>)> = sqlx::query_as(
                            "SELECT id, revision FROM capability_pack_versions \
                             WHERE pack_id = $1 AND status = 'deprecated' AND manifest IS NOT NULL \
                             ORDER BY retired_at DESC NULLS LAST, id DESC OFFSET $2",
                        )
                        .bind(&pack_id)
                        .bind(retain)
                        .fetch_all(&mut **tx)
                        .await?;
                        for (id, old_revision) in beyond {
                            sqlx::query(
                                "UPDATE capability_pack_versions SET status = 'withdrawn', manifest = NULL, \
                                 files = '{}'::jsonb WHERE id = $1",
                            )
                            .bind(&id)
                            .execute(&mut **tx)
                            .await?;
                            evicted.extend(old_revision);
                        }
                        version_id
                    };
                    let report = sqlx::query(&format!(
                        "UPDATE pack_install_reports SET state = 'succeeded', phase = 'done', outcome = 'activated', \
                         pack_id = $3, pack_name = $4, revision = $5, previous_revision = $6, diagnostics = $7, \
                         diagnostics_omitted = $8, finished_at = now() \
                         WHERE tenant_id = $1 AND request_id = $2 RETURNING {REPORT_COLUMNS}"
                    ))
                    .bind(&tenant)
                    .bind(&request)
                    .bind(&pack_id)
                    .bind(&name)
                    .bind(&revision)
                    .bind(&current_revision)
                    .bind(&stored)
                    .bind(i32::try_from(omitted).unwrap_or(i32::MAX))
                    .fetch_one(&mut **tx)
                    .await?;
                    let version = next_version(tx, batch, &tenant, &pack_id).await?;
                    batch.emit(
                        EventDraft::new(
                            "pack",
                            pack_id.clone(),
                            version,
                            event_type("pack.installed")?,
                            correlation_id,
                            actor,
                        )
                        .with_payload(json!({
                            "request_id": request,
                            "pack": name,
                            "pack_id": pack_id,
                            "version_id": version_id,
                            "revision": revision,
                            "previous_revision": current_revision,
                            "semver": semver,
                            "unchanged": unchanged,
                            "evicted": evicted,
                            "resolution": resolution,
                        })),
                    );
                    Ok((pack_id, version_id, current_revision, evicted, unchanged, report))
                })
            })
            .await?;
        let (pack_id, version_id, previous_revision, evicted, unchanged, row) = outcome;
        Ok(Activated {
            pack_id,
            version_id,
            revision: activation.revision.to_string(),
            previous_revision,
            evicted,
            unchanged,
            report: report_of(&row)?,
        })
    }

    /// Activate the most recently retired revision that still has its content; the active one is retained in its
    /// place, so a second rollback goes forward again.
    ///
    /// # Errors
    /// [`PackError::NotFound`], [`PackError::NoPrevious`], [`PackError::Conflict`] and database failures.
    pub async fn rollback(
        &self,
        tenant_id: &str,
        name: &str,
        expected_active: Option<Option<&str>>,
        actor: &Actor,
        correlation_id: CorrelationId,
    ) -> Result<RolledBack, PackError> {
        let tenant = tenant_id.to_string();
        let name = name.to_string();
        let expected: Option<Option<String>> = expected_active.map(|e| e.map(str::to_string));
        let actor = actor.clone();
        self.commit(tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let Some(pack) = sqlx::query(
                        "SELECT id, current_published_version_id FROM capability_packs \
                         WHERE tenant_id = $1 AND name = $2 FOR UPDATE",
                    )
                    .bind(&tenant)
                    .bind(&name)
                    .fetch_optional(&mut **tx)
                    .await?
                    else {
                        return Err(PackError::NotFound { pack: name });
                    };
                    let pack_id: String = pack.try_get("id")?;
                    let current: Option<String> = pack.try_get("current_published_version_id")?;
                    let current_revision: Option<String> = match &current {
                        Some(id) => sqlx::query_scalar(
                            "SELECT revision FROM capability_pack_versions WHERE id = $1",
                        )
                        .bind(id)
                        .fetch_optional(&mut **tx)
                        .await?
                        .flatten(),
                        None => None,
                    };
                    if let Some(expected) = &expected {
                        if expected != &current_revision {
                            return Err(PackError::Conflict {
                                pack: name,
                                expected: expected.clone(),
                                current: current_revision,
                            });
                        }
                    }
                    let previous: Option<(String, String)> = sqlx::query_as(
                        "SELECT id, revision FROM capability_pack_versions \
                         WHERE pack_id = $1 AND status = 'deprecated' AND manifest IS NOT NULL \
                           AND revision IS NOT NULL \
                         ORDER BY retired_at DESC NULLS LAST, id DESC LIMIT 1",
                    )
                    .bind(&pack_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some((previous_id, previous_revision)) = previous else {
                        return Err(PackError::NoPrevious { pack: name });
                    };
                    if let Some(current) = &current {
                        sqlx::query(
                            "UPDATE capability_pack_versions SET status = 'deprecated', retired_at = now() WHERE id = $1",
                        )
                        .bind(current)
                        .execute(&mut **tx)
                        .await?;
                    }
                    sqlx::query(
                        "UPDATE capability_pack_versions SET status = 'published', activated_at = now(), \
                         retired_at = NULL WHERE id = $1",
                    )
                    .bind(&previous_id)
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query(
                        "UPDATE capability_packs SET current_published_version_id = $2 WHERE id = $1",
                    )
                    .bind(&pack_id)
                    .bind(&previous_id)
                    .execute(&mut **tx)
                    .await?;
                    let version = next_version(tx, batch, &tenant, &pack_id).await?;
                    batch.emit(
                        EventDraft::new(
                            "pack",
                            pack_id.clone(),
                            version,
                            event_type("pack.rolled_back")?,
                            correlation_id,
                            actor,
                        )
                        .with_payload(json!({
                            "pack": name,
                            "pack_id": pack_id,
                            "from_revision": current_revision,
                            "to_revision": previous_revision,
                            "version_id": previous_id,
                        })),
                    );
                    Ok(RolledBack {
                        pack_id,
                        name,
                        from_revision: current_revision,
                        to_revision: previous_revision,
                        version_id: previous_id,
                    })
                })
            })
            .await
    }

    /// Every pack of the tenant with its revisions.
    ///
    /// # Errors
    /// A database failure.
    pub async fn packs(&self, tenant_id: &str) -> Result<Vec<PackSummary>, PackError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let packs = sqlx::query(
            "SELECT id, name, current_published_version_id FROM capability_packs \
             WHERE tenant_id = $1 ORDER BY name",
        )
        .bind(tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut out = Vec::with_capacity(packs.len());
        for pack in packs {
            let pack_id: String = pack.try_get("id")?;
            let current: Option<String> = pack.try_get("current_published_version_id")?;
            let rows = sqlx::query(
                "SELECT id, revision, semver, status, manifest, activated_at, retired_at \
                 FROM capability_pack_versions WHERE pack_id = $1 AND revision IS NOT NULL \
                 ORDER BY activated_at DESC NULLS LAST, id DESC",
            )
            .bind(&pack_id)
            .fetch_all(&mut *tx)
            .await?;
            let mut active = None;
            let mut revisions = Vec::new();
            for row in &rows {
                let info = revision_of_row(row)?;
                if current.as_deref() == Some(info.version_id.as_str()) {
                    active = Some(info);
                } else {
                    revisions.push(info);
                }
            }
            out.push(PackSummary {
                pack_id,
                name: pack.try_get("name")?,
                active,
                revisions,
            });
        }
        tx.commit().await?;
        Ok(out)
    }

    /// The tenant's install reports, newest first. Expired reports are not returned (and are purged by the
    /// sweep the next install or read runs); a report left `running` past its grace is closed first.
    ///
    /// # Errors
    /// A database failure.
    pub async fn reports(
        &self,
        tenant_id: &str,
        pack: Option<&str>,
        limit: i64,
    ) -> Result<Vec<InstallReport>, PackError> {
        let limit = limit.clamp(1, self.config.reports.max_listed);
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        self.sweep(&mut tx, tenant_id).await?;
        let rows = sqlx::query(&format!(
            "SELECT {REPORT_COLUMNS} FROM pack_install_reports \
             WHERE tenant_id = $1 AND expires_at > now() AND ($2::text IS NULL OR pack_name = $2) \
             ORDER BY started_at DESC, request_id DESC LIMIT $3"
        ))
        .bind(tenant_id)
        .bind(pack)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(report_of).collect()
    }

    /// One report, if it exists and has not expired.
    ///
    /// # Errors
    /// A database failure.
    pub async fn report(
        &self,
        tenant_id: &str,
        request_id: &str,
    ) -> Result<Option<InstallReport>, PackError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let row = sqlx::query(&format!(
            "SELECT {REPORT_COLUMNS} FROM pack_install_reports \
             WHERE tenant_id = $1 AND request_id = $2 AND expires_at > now()"
        ))
        .bind(tenant_id)
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(report_of).transpose()
    }

    /// Whether any diagnostic of a finished report is an error (used to describe a replay).
    #[must_use]
    pub fn has_errors(report: &InstallReport) -> bool {
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}
