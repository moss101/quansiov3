//! The durable recorder-session row and its fence (ADP-003, DOMAIN.md §11.7 A, B).
//!
//! [`RecordingStore`] owns `skill_recordings`. Every mutation is one transaction that also stages its
//! `recording.*` RuntimeEvent, so the row and the event trail cannot disagree. Three things are the
//! store's rather than the caller's:
//!
//! * **the person finishes it.** `recording → finished` happens in exactly one place, [`RecordingStore::finish`],
//!   which needs the person's own call (the API refuses any other actor before it gets here), the archive
//!   they built and a recording still in progress;
//! * **the owner fence.** Every call names its user, session family and epoch. A call that does not match
//!   is `RECORDING_OWNER_CHANGED`, and when it comes from the owner's own stale session or epoch it also
//!   aborts a recording in progress in the same transaction (a call from another *user* only fails: a
//!   stranger must not be able to end someone else's demonstration);
//! * **no timer.** An unfinished recording past `expires_at` is aborted the next time anything touches it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, CorrelationId, Digest, Prefix, UlidGenerator};
use quansio_events::event_type::EventType;
use quansio_events::{Actor, EventBatch, EventDraft, EventError, EventStore};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::archive::{self, ARCHIVE_MEDIA_TYPE};
use super::config::RecordingConfig;
use super::{RecordingError, RECORDING_OWNER};
use crate::artifacts::{
    ArtifactGrant, ArtifactKind, ArtifactStore, GrantLevel, NewArtifact, NewVersion, ObjectStore,
    Origin, OriginKind, ProducedBy, Retention,
};

const COLUMNS: &str = "id, tenant_id, workspace_id, owner_user_id, session_family_id, epoch, status, \
    start_key, started_at, expires_at, (expires_at <= now()) AS expired, finished_at, artifact_id, \
    artifact_version_id, content_digest, size_bytes, redaction, stats, proposal, skill_version_id, \
    abort_reason, created_at, updated_at";

/// What a recording is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingStatus {
    /// The person is demonstrating.
    Recording,
    /// The person finished; the archive is stored.
    Finished,
    /// A proposal has been compiled and is under review.
    Compiled,
    /// The person submitted a `DRAFT` skill.
    Submitted,
    /// The person cancelled.
    Cancelled,
    /// An owner-fence failure or expiry ended it.
    Aborted,
}

impl RecordingStatus {
    /// The durable spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Finished => "finished",
            Self::Compiled => "compiled",
            Self::Submitted => "submitted",
            Self::Cancelled => "cancelled",
            Self::Aborted => "aborted",
        }
    }

    /// Parse the durable spelling.
    ///
    /// # Errors
    /// [`RecordingError::Malformed`] for a value outside the vocabulary.
    pub fn parse(value: &str) -> Result<Self, RecordingError> {
        [
            Self::Recording,
            Self::Finished,
            Self::Compiled,
            Self::Submitted,
            Self::Cancelled,
            Self::Aborted,
        ]
        .into_iter()
        .find(|status| status.as_str() == value)
        .ok_or_else(|| RecordingError::Malformed(format!("{value:?} is not a recording status")))
    }
}

/// Who is calling: the authenticated person, their session and the epoch they carry.
#[derive(Debug, Clone)]
pub struct RecordingCaller {
    /// Tenant, from the authenticated session.
    pub tenant_id: String,
    /// The person (`usr_…`), from the authenticated session.
    pub user_id: String,
    /// The sign-in the call arrives under (a refresh rotation stays in its family); `None` only
    /// where scope comes from trusted headers.
    pub session_family_id: Option<String>,
    /// The epoch the desktop carries (§11.7 B).
    pub epoch: i64,
    /// Everything caused by one external input shares this id.
    pub correlation_id: CorrelationId,
}

/// A persisted `skill_recordings` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// `rec_…`.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Workspace.
    pub workspace_id: String,
    /// The person.
    pub owner_user_id: String,
    /// The sign-in family it started under.
    pub session_family_id: Option<String>,
    /// The epoch it started under.
    pub epoch: i64,
    /// State.
    pub status: RecordingStatus,
    /// Idempotency key of the start.
    pub start_key: String,
    /// When it started.
    pub started_at: DateTime<Utc>,
    /// When an unfinished recording is aborted.
    pub expires_at: DateTime<Utc>,
    /// Whether `expires_at` has passed, by the database's clock.
    pub expired: bool,
    /// When the person finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// The archive's artifact.
    pub artifact_id: Option<String>,
    /// The archive's artifact version.
    pub artifact_version_id: Option<String>,
    /// SHA-256 of the archive.
    pub content_digest: Option<String>,
    /// Archive size.
    pub size_bytes: Option<i64>,
    /// `{rule_set, counts, screenshots_withheld}`.
    pub redaction: Option<Value>,
    /// `{actions, screenshots, transcript_segments, paused_ms}`.
    pub stats: Option<Value>,
    /// The compiled proposal under review.
    pub proposal: Option<Value>,
    /// The `DRAFT` skill version it produced.
    pub skill_version_id: Option<String>,
    /// Why it was aborted.
    pub abort_reason: Option<String>,
    /// Row creation.
    pub created_at: DateTime<Utc>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

impl Recording {
    /// The projection: never the archive, and the proposal only when asked for.
    #[must_use]
    pub fn to_json(&self, with_proposal: bool) -> Value {
        let mut value = json!({
            "id": self.id,
            "workspace_id": self.workspace_id,
            "status": self.status.as_str(),
            "epoch": self.epoch,
            "started_at": self.started_at,
            "expires_at": self.expires_at,
            "expired": self.expired && self.status == RecordingStatus::Recording,
            "finished_at": self.finished_at,
            "artifact_id": self.artifact_id,
            "artifact_version_id": self.artifact_version_id,
            "content_digest": self.content_digest,
            "size_bytes": self.size_bytes,
            "redaction": self.redaction,
            "stats": self.stats,
            "skill_version_id": self.skill_version_id,
            "abort_reason": self.abort_reason,
            "has_proposal": self.proposal.is_some(),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        });
        if with_proposal {
            value["proposal"] = self.proposal.clone().unwrap_or(Value::Null);
        }
        value
    }
}

/// What a mutation decided after it changed state: the value, or a refusal that is returned once the
/// state change (an abort) has committed.
type Step<T> = Result<T, RecordingError>;
type BoxStepFuture<'a, T> = Pin<Box<dyn Future<Output = Result<Step<T>, RecordingError>> + Send + 'a>>;

/// The durable recorder-session registry.
#[derive(Debug, Clone)]
pub struct RecordingStore {
    events: EventStore,
}

impl RecordingStore {
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

    /// Start a recording: one per person at a time, idempotent by `start_key`.
    ///
    /// # Errors
    /// [`RecordingError::State`] when the person already has a recording in progress,
    /// [`RecordingError::IdempotencyMismatch`] when the key was used with other arguments and
    /// [`RecordingError::OwnerChanged`] when it was used by another sign-in.
    pub async fn start(
        &self,
        caller: &RecordingCaller,
        workspace_id: &str,
        start_key: &str,
        config: &RecordingConfig,
    ) -> Result<Recording, RecordingError> {
        if start_key.is_empty() || start_key.chars().count() > 128 || start_key.chars().any(char::is_control) {
            return Err(RecordingError::Invalid("start_key must be 1..=128 printable characters".to_string()));
        }
        if caller.epoch < 1 {
            return Err(RecordingError::Invalid("epoch must be a positive integer".to_string()));
        }
        if let Some(existing) = self.find_by_key(caller, start_key).await? {
            return replay_of_start(existing, caller, workspace_id);
        }
        let duration_ms = i64::try_from(config.max_duration.as_millis()).unwrap_or(i64::MAX);
        let owned = caller.clone();
        let workspace_owned = workspace_id.to_string();
        let key_owned = start_key.to_string();
        let attempt = self
            .commit(caller, move |tx, batch| {
                let caller = owned;
                let workspace_id = workspace_owned;
                let start_key = key_owned;
                Box::pin(async move {
                    abort_expired(tx, batch, &caller).await?;
                    let busy: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM skill_recordings WHERE tenant_id = $1 \
                         AND owner_user_id = $2 AND status = 'recording')",
                    )
                    .bind(&caller.tenant_id)
                    .bind(&caller.user_id)
                    .fetch_one(&mut **tx)
                    .await?;
                    if busy {
                        return Ok(Err(RecordingError::State {
                            id: String::new(),
                            status: "recording".to_string(),
                            detail: "finish or cancel the recording in progress before starting another".to_string(),
                        }));
                    }
                    let id = CanonicalId::generate(Prefix::Recording, &mut UlidGenerator::new()).to_string();
                    let row = sqlx::query(&format!(
                        "INSERT INTO skill_recordings (id, tenant_id, workspace_id, owner_user_id, \
                         session_family_id, epoch, start_key, expires_at) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, now() + ($8::bigint * interval '1 millisecond')) \
                         RETURNING {COLUMNS}"
                    ))
                    .bind(&id)
                    .bind(&caller.tenant_id)
                    .bind(&workspace_id)
                    .bind(&caller.user_id)
                    .bind(&caller.session_family_id)
                    .bind(caller.epoch)
                    .bind(&start_key)
                    .bind(duration_ms)
                    .fetch_one(&mut **tx)
                    .await?;
                    let recording = map_row(&row)?;
                    stage(
                        tx,
                        batch,
                        &caller,
                        &recording.id,
                        "recording.started",
                        json!({
                            "recording_id": recording.id,
                            "workspace_id": recording.workspace_id,
                            "epoch": recording.epoch,
                            "expires_at": recording.expires_at,
                        }),
                    )
                    .await?;
                    Ok(Ok(recording))
                })
            })
            .await;
        match attempt {
            Err(RecordingError::Database(sqlx::Error::Database(error))) if error.is_unique_violation() => {
                // Two starts raced: the same key is a replay, another key is a recording in progress.
                match self.find_by_key(caller, start_key).await? {
                    Some(existing) => replay_of_start(existing, caller, workspace_id),
                    None => Err(RecordingError::State {
                        id: String::new(),
                        status: "recording".to_string(),
                        detail: "finish or cancel the recording in progress before starting another".to_string(),
                    }),
                }
            }
            other => other,
        }
    }

    /// Finish a recording with the archive the person's recorder built: the only way out of `recording`
    /// into `finished`.
    ///
    /// The archive is validated (shape, limits, ownership, nothing left for redaction to remove) before
    /// anything is stored; a refusal stores nothing and leaves the recording in progress.
    ///
    /// # Errors
    /// [`RecordingError::OwnerChanged`] for a failed fence, [`RecordingError::State`] for a recording that
    /// is not in progress or has expired, and the archive's refusals.
    pub async fn finish(
        &self,
        caller: &RecordingCaller,
        objects: Arc<dyn ObjectStore>,
        recording_id: &str,
        archive_bytes: &[u8],
        claimed_digest: &str,
        config: &'static RecordingConfig,
    ) -> Result<Recording, RecordingError> {
        let digest = Digest::of(archive_bytes);
        if digest.as_str() != claimed_digest {
            return Err(RecordingError::InvalidArchive(
                "content_digest does not match the archive's bytes".to_string(),
            ));
        }
        if let Some(existing) = self.peek_for_replay(caller, recording_id).await? {
            if matches!(
                existing.status,
                RecordingStatus::Finished | RecordingStatus::Compiled | RecordingStatus::Submitted
            ) && existing.content_digest.as_deref() == Some(digest.as_str())
            {
                return Ok(existing);
            }
        }
        let recording_id = recording_id.to_string();
        let owned = caller.clone();
        let bytes_owned = archive_bytes.to_vec();
        let digest_owned = digest.clone();
        self.commit(caller, move |tx, batch| {
            let caller = owned;
            let bytes = bytes_owned;
            let digest = digest_owned;
            Box::pin(async move {
                let row = load_locked(tx, &caller.tenant_id, &recording_id).await?;
                if let Some(refusal) = fence(tx, batch, &row, &caller).await? {
                    return Ok(Err(refusal));
                }
                if row.status != RecordingStatus::Recording {
                    return Ok(Err(not_in_progress(&row, "finish")));
                }
                if row.expired {
                    expire(tx, batch, &caller, &row).await?;
                    return Ok(Err(RecordingError::State {
                        id: row.id.clone(),
                        status: "aborted".to_string(),
                        detail: "the recording expired before it was finished".to_string(),
                    }));
                }
                let report = archive::validate(
                    &bytes,
                    &archive::Expected {
                        recording_id: &row.id,
                        tenant_id: &caller.tenant_id,
                        user_id: &caller.user_id,
                        epoch: row.epoch,
                    },
                    config,
                )?;
                let artifacts = ArtifactStore::new(objects.as_ref());
                let artifact = artifacts
                    .create(
                        &mut **tx,
                        &caller.tenant_id,
                        &caller.user_id,
                        &NewArtifact {
                            workspace_id: row.workspace_id.clone(),
                            kind: ArtifactKind::Archive,
                            title: format!("Skill recording {}", row.id),
                            role: Some("recording".to_string()),
                            origin: Origin {
                                kind: OriginKind::Upload,
                                reference: row.id.clone(),
                            },
                            grants: vec![ArtifactGrant {
                                principal: caller.user_id.clone(),
                                level: GrantLevel::Write,
                            }],
                            retention: Retention::default(),
                        },
                    )
                    .await?;
                let stored = artifacts
                    .add_version(
                        &mut **tx,
                        &caller.tenant_id,
                        &caller.user_id,
                        &artifact.id,
                        &NewVersion {
                            bytes: bytes.clone(),
                            media_type: ARCHIVE_MEDIA_TYPE.to_string(),
                            expected_digest: Some(digest.clone()),
                            produced_by: ProducedBy {
                                run_id: None,
                                step_id: None,
                                user_id: Some(caller.user_id.clone()),
                            },
                        },
                    )
                    .await?;
                let updated = sqlx::query(&format!(
                    "UPDATE skill_recordings SET status = 'finished', finished_at = now(), \
                     artifact_id = $3, artifact_version_id = $4, content_digest = $5, size_bytes = $6, \
                     redaction = $7, stats = $8 \
                     WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
                ))
                .bind(&caller.tenant_id)
                .bind(&row.id)
                .bind(artifact.id.to_canonical())
                .bind(stored.version.id.to_canonical())
                .bind(digest.as_str())
                .bind(i64::try_from(bytes.len()).unwrap_or(i64::MAX))
                .bind(&report.redaction)
                .bind(&report.stats)
                .fetch_one(&mut **tx)
                .await?;
                let recording = map_row(&updated)?;
                stage(
                    tx,
                    batch,
                    &caller,
                    &recording.id,
                    "recording.finished",
                    json!({
                        "recording_id": recording.id,
                        "artifact_id": recording.artifact_id,
                        "artifact_version_id": recording.artifact_version_id,
                        "content_digest": recording.content_digest,
                        "size_bytes": recording.size_bytes,
                        "redaction": recording.redaction,
                        "stats": recording.stats,
                    }),
                )
                .await?;
                Ok(Ok(recording))
            })
        })
        .await
    }

    /// Cancel a recording in progress: nothing was ever stored for it.
    ///
    /// # Errors
    /// [`RecordingError::OwnerChanged`], or [`RecordingError::State`] for one that is not in progress.
    pub async fn cancel(&self, caller: &RecordingCaller, recording_id: &str) -> Result<Recording, RecordingError> {
        if let Some(existing) = self.peek_for_replay(caller, recording_id).await? {
            if existing.status == RecordingStatus::Cancelled {
                return Ok(existing);
            }
        }
        let recording_id = recording_id.to_string();
        let owned = caller.clone();
        self.commit(caller, move |tx, batch| {
            let caller = owned;
            Box::pin(async move {
                let row = load_locked(tx, &caller.tenant_id, &recording_id).await?;
                if let Some(refusal) = fence(tx, batch, &row, &caller).await? {
                    return Ok(Err(refusal));
                }
                if row.status != RecordingStatus::Recording {
                    return Ok(Err(not_in_progress(&row, "cancel")));
                }
                let updated = sqlx::query(&format!(
                    "UPDATE skill_recordings SET status = 'cancelled' WHERE tenant_id = $1 AND id = $2 \
                     RETURNING {COLUMNS}"
                ))
                .bind(&caller.tenant_id)
                .bind(&row.id)
                .fetch_one(&mut **tx)
                .await?;
                let recording = map_row(&updated)?;
                stage(
                    tx,
                    batch,
                    &caller,
                    &recording.id,
                    "recording.cancelled",
                    json!({ "recording_id": recording.id }),
                )
                .await?;
                Ok(Ok(recording))
            })
        })
        .await
    }

    /// The recording a compile or a submission works on: the fence holds and the person has finished.
    ///
    /// # Errors
    /// [`RecordingError::OwnerChanged`], [`RecordingError::NotFinished`] for a recording still in
    /// progress, [`RecordingError::State`] for one that ended without being finished or already became a
    /// skill (when `allow_submitted` is false).
    pub async fn require_finished(
        &self,
        caller: &RecordingCaller,
        recording_id: &str,
        wanted: &[RecordingStatus],
    ) -> Result<Recording, RecordingError> {
        let Some(row) = self.peek(caller, recording_id).await? else {
            return Err(RecordingError::NotFound {
                id: recording_id.to_string(),
            });
        };
        if row.status == RecordingStatus::Recording {
            return Err(RecordingError::NotFinished { id: row.id });
        }
        if !wanted.contains(&row.status) {
            return Err(RecordingError::State {
                id: row.id.clone(),
                status: row.status.as_str().to_string(),
                detail: format!(
                    "this call needs the recording to be {}",
                    wanted.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" or ")
                ),
            });
        }
        Ok(row)
    }

    /// Keep the validated proposal on the row: `finished` or `compiled` becomes `compiled`.
    ///
    /// # Errors
    /// As [`RecordingStore::require_finished`].
    pub async fn record_proposal(
        &self,
        caller: &RecordingCaller,
        recording_id: &str,
        proposal: &Value,
    ) -> Result<Recording, RecordingError> {
        let recording_id = recording_id.to_string();
        let owned = caller.clone();
        let proposal_owned = proposal.clone();
        self.commit(caller, move |tx, batch| {
            let caller = owned;
            let proposal = proposal_owned;
            Box::pin(async move {
                let row = load_locked(tx, &caller.tenant_id, &recording_id).await?;
                if let Some(refusal) = fence(tx, batch, &row, &caller).await? {
                    return Ok(Err(refusal));
                }
                match row.status {
                    RecordingStatus::Recording => return Ok(Err(RecordingError::NotFinished { id: row.id })),
                    RecordingStatus::Finished | RecordingStatus::Compiled => {}
                    _ => return Ok(Err(not_in_progress(&row, "compile"))),
                }
                let updated = sqlx::query(&format!(
                    "UPDATE skill_recordings SET status = 'compiled', proposal = $3 \
                     WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
                ))
                .bind(&caller.tenant_id)
                .bind(&row.id)
                .bind(&proposal)
                .fetch_one(&mut **tx)
                .await?;
                let recording = map_row(&updated)?;
                stage(
                    tx,
                    batch,
                    &caller,
                    &recording.id,
                    "recording.compiled",
                    json!({
                        "recording_id": recording.id,
                        "slots": proposal["slots"].as_array().map_or(0, Vec::len),
                        "steps": proposal["manifest"]["procedure"]["steps"].as_array().map_or(0, Vec::len),
                    }),
                )
                .await?;
                Ok(Ok(recording))
            })
        })
        .await
    }

    /// Record the `DRAFT` skill version a submission produced: `compiled` becomes `submitted`.
    ///
    /// # Errors
    /// As [`RecordingStore::require_finished`]; a recording already submitted with the same version is
    /// returned unchanged.
    pub async fn record_submission(
        &self,
        caller: &RecordingCaller,
        recording_id: &str,
        skill_version_id: &str,
    ) -> Result<Recording, RecordingError> {
        let recording_id = recording_id.to_string();
        let skill_version_id = skill_version_id.to_string();
        if let Some(existing) = self.peek_for_replay(caller, &recording_id).await? {
            if existing.status == RecordingStatus::Submitted
                && existing.skill_version_id.as_deref() == Some(skill_version_id.as_str())
            {
                return Ok(existing);
            }
        }
        let owned = caller.clone();
        self.commit(caller, move |tx, batch| {
            let caller = owned;
            Box::pin(async move {
                let row = load_locked(tx, &caller.tenant_id, &recording_id).await?;
                if let Some(refusal) = fence(tx, batch, &row, &caller).await? {
                    return Ok(Err(refusal));
                }
                if row.status != RecordingStatus::Compiled {
                    return Ok(Err(not_in_progress(&row, "submit")));
                }
                let updated = sqlx::query(&format!(
                    "UPDATE skill_recordings SET status = 'submitted', skill_version_id = $3 \
                     WHERE tenant_id = $1 AND id = $2 RETURNING {COLUMNS}"
                ))
                .bind(&caller.tenant_id)
                .bind(&row.id)
                .bind(&skill_version_id)
                .fetch_one(&mut **tx)
                .await?;
                let recording = map_row(&updated)?;
                stage(
                    tx,
                    batch,
                    &caller,
                    &recording.id,
                    "recording.submitted",
                    json!({
                        "recording_id": recording.id,
                        "skill_version_id": recording.skill_version_id,
                    }),
                )
                .await?;
                Ok(Ok(recording))
            })
        })
        .await
    }

    /// One of the caller's own recordings, or `None`. A read: it applies no fence and changes nothing.
    ///
    /// # Errors
    /// A database failure.
    pub async fn get_owned(
        &self,
        tenant_id: &str,
        user_id: &str,
        recording_id: &str,
    ) -> Result<Option<Recording>, RecordingError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM skill_recordings WHERE tenant_id = $1 AND id = $2 AND owner_user_id = $3"
        ))
        .bind(tenant_id)
        .bind(recording_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(map_row).transpose()
    }

    /// The caller's own recordings, newest first.
    ///
    /// # Errors
    /// A database failure.
    pub async fn list_owned(&self, tenant_id: &str, user_id: &str, limit: i64) -> Result<Vec<Recording>, RecordingError> {
        let mut tx = self.events.begin_tenant_transaction(tenant_id).await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM skill_recordings WHERE tenant_id = $1 AND owner_user_id = $2 \
             ORDER BY created_at DESC, id DESC LIMIT $3"
        ))
        .bind(tenant_id)
        .bind(user_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(map_row).collect()
    }

    /// A recording as its owner's call sees it: `None` when it does not exist, the fence's refusal
    /// (without aborting anything: this is a read) when the call is not the owner's.
    async fn peek(&self, caller: &RecordingCaller, recording_id: &str) -> Result<Option<Recording>, RecordingError> {
        let mut tx = self.events.begin_tenant_transaction(&caller.tenant_id).await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM skill_recordings WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(&caller.tenant_id)
        .bind(recording_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let Some(row) = row.as_ref().map(map_row).transpose()? else {
            return Ok(None);
        };
        match violation(&row, caller) {
            Some(reason) => Err(RecordingError::OwnerChanged { reason }),
            None => Ok(Some(row)),
        }
    }

    /// Like [`Self::peek`], but a failed fence is `None`: the mutation that follows applies the fence
    /// itself (and records an abort), so a replay check must not pre-empt it.
    async fn peek_for_replay(
        &self,
        caller: &RecordingCaller,
        recording_id: &str,
    ) -> Result<Option<Recording>, RecordingError> {
        match self.peek(caller, recording_id).await {
            Err(RecordingError::OwnerChanged { .. }) => Ok(None),
            other => other,
        }
    }

    async fn find_by_key(&self, caller: &RecordingCaller, start_key: &str) -> Result<Option<Recording>, RecordingError> {
        let mut tx = self.events.begin_tenant_transaction(&caller.tenant_id).await?;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM skill_recordings \
             WHERE tenant_id = $1 AND owner_user_id = $2 AND start_key = $3"
        ))
        .bind(&caller.tenant_id)
        .bind(&caller.user_id)
        .bind(start_key)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.as_ref().map(map_row).transpose()
    }

    /// Run one mutation that must commit a `recording.*` event with it.
    ///
    /// The closure's outer `Err` rolls the transaction back; its inner `Err` is a refusal that is
    /// returned after the transaction committed (an abort the fence recorded is kept).
    async fn commit<T, F>(&self, caller: &RecordingCaller, mutation: F) -> Result<T, RecordingError>
    where
        T: Send,
        F: Send
            + 'static
            + for<'a> FnOnce(&'a mut Transaction<'static, Postgres>, &'a mut EventBatch) -> BoxStepFuture<'a, T>,
    {
        let rejection: Arc<Mutex<Option<RecordingError>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&rejection);
        let result = self
            .events
            .commit_mutation_tx(&caller.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let rejected = |error: RecordingError| {
                        let message = error.to_string();
                        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                        EventError::MutationRejected {
                            owner: RECORDING_OWNER,
                            message,
                        }
                    };
                    match mutation(&mut *tx, &mut *batch).await {
                        Ok(Ok(value)) => Ok(Ok(value)),
                        // A refusal that staged an event (an abort the fence recorded) commits that
                        // and is returned afterwards; one that staged nothing rolls back.
                        Ok(Err(refusal)) if batch.is_empty() => Err(rejected(refusal)),
                        Ok(Err(refusal)) => Ok(Err(refusal)),
                        Err(error) => Err(rejected(error)),
                    }
                })
            })
            .await;
        match result {
            Ok(step) => step,
            Err(error) => match rejection.lock().unwrap_or_else(PoisonError::into_inner).take() {
                Some(error) => Err(error),
                None => Err(RecordingError::Event(error)),
            },
        }
    }
}

fn replay_of_start(existing: Recording, caller: &RecordingCaller, workspace_id: &str) -> Result<Recording, RecordingError> {
    if existing.workspace_id != workspace_id || existing.epoch != caller.epoch {
        return Err(RecordingError::IdempotencyMismatch {
            key: existing.start_key,
        });
    }
    if let Some(reason) = violation(&existing, caller) {
        return Err(RecordingError::OwnerChanged { reason });
    }
    Ok(existing)
}

fn not_in_progress(row: &Recording, call: &str) -> RecordingError {
    RecordingError::State {
        id: row.id.clone(),
        status: row.status.as_str().to_string(),
        detail: format!("{call} is not allowed from this state"),
    }
}

/// Which part of the owner fence a call fails, if any: user first, then session family, then epoch.
#[must_use]
pub fn violation(row: &Recording, caller: &RecordingCaller) -> Option<&'static str> {
    if row.owner_user_id != caller.user_id || row.tenant_id != caller.tenant_id {
        return Some("the call is from another user");
    }
    if let Some(family) = &row.session_family_id {
        if caller.session_family_id.as_ref() != Some(family) {
            return Some("the call is from another sign-in session");
        }
    }
    if row.epoch != caller.epoch {
        return Some("the call carries another epoch");
    }
    None
}

/// Apply the fence inside a mutation. A failed fence from the owner's own stale session or epoch
/// aborts a recording in progress (and commits that), a failed fence from another user only refuses.
async fn fence(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    row: &Recording,
    caller: &RecordingCaller,
) -> Result<Option<RecordingError>, RecordingError> {
    let Some(reason) = violation(row, caller) else {
        return Ok(None);
    };
    let same_user = row.owner_user_id == caller.user_id && row.tenant_id == caller.tenant_id;
    if same_user && row.status == RecordingStatus::Recording {
        sqlx::query(
            "UPDATE skill_recordings SET status = 'aborted', abort_reason = 'owner_changed' \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&row.tenant_id)
        .bind(&row.id)
        .execute(&mut **tx)
        .await?;
        stage(
            tx,
            batch,
            caller,
            &row.id,
            "recording.aborted",
            json!({ "recording_id": row.id, "reason": "owner_changed", "detail": reason }),
        )
        .await?;
    }
    Ok(Some(RecordingError::OwnerChanged { reason }))
}

/// Abort every unfinished recording of the caller that is past its lifetime.
async fn abort_expired(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    caller: &RecordingCaller,
) -> Result<(), RecordingError> {
    let ids: Vec<String> = sqlx::query_scalar(
        "UPDATE skill_recordings SET status = 'aborted', abort_reason = 'expired' \
         WHERE tenant_id = $1 AND owner_user_id = $2 AND status = 'recording' AND expires_at <= now() \
         RETURNING id",
    )
    .bind(&caller.tenant_id)
    .bind(&caller.user_id)
    .fetch_all(&mut **tx)
    .await?;
    for id in ids {
        stage(
            tx,
            batch,
            caller,
            &id,
            "recording.aborted",
            json!({ "recording_id": id, "reason": "expired" }),
        )
        .await?;
    }
    Ok(())
}

async fn expire(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    caller: &RecordingCaller,
    row: &Recording,
) -> Result<(), RecordingError> {
    sqlx::query(
        "UPDATE skill_recordings SET status = 'aborted', abort_reason = 'expired' \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&row.tenant_id)
    .bind(&row.id)
    .execute(&mut **tx)
    .await?;
    stage(
        tx,
        batch,
        caller,
        &row.id,
        "recording.aborted",
        json!({ "recording_id": row.id, "reason": "expired" }),
    )
    .await
}

async fn load_locked(
    tx: &mut Transaction<'static, Postgres>,
    tenant_id: &str,
    recording_id: &str,
) -> Result<Recording, RecordingError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM skill_recordings WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(recording_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| RecordingError::NotFound {
        id: recording_id.to_string(),
    })?;
    map_row(&row)
}

/// Stage one `recording.*` event at the aggregate's next version.
async fn stage(
    tx: &mut Transaction<'static, Postgres>,
    batch: &mut EventBatch,
    caller: &RecordingCaller,
    recording_id: &str,
    event_type: &str,
    payload: Value,
) -> Result<(), RecordingError> {
    let committed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 \
         AND aggregate_type = 'recording' AND aggregate_id = $2",
    )
    .bind(&caller.tenant_id)
    .bind(recording_id)
    .fetch_one(&mut **tx)
    .await?;
    let staged = batch.staged_version("recording", recording_id).unwrap_or(0);
    let version = u64::try_from(committed).unwrap_or(0).max(staged) + 1;
    batch.emit(
        EventDraft::new(
            "recording",
            recording_id,
            version,
            EventType::parse(event_type)?,
            caller.correlation_id,
            Actor::user(&caller.user_id),
        )
        .with_payload(payload),
    );
    Ok(())
}

fn map_row(row: &PgRow) -> Result<Recording, RecordingError> {
    Ok(Recording {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        workspace_id: row.try_get("workspace_id")?,
        owner_user_id: row.try_get("owner_user_id")?,
        session_family_id: row.try_get("session_family_id")?,
        epoch: row.try_get("epoch")?,
        status: RecordingStatus::parse(&row.try_get::<String, _>("status")?)?,
        start_key: row.try_get("start_key")?,
        started_at: row.try_get("started_at")?,
        expires_at: row.try_get("expires_at")?,
        expired: row.try_get("expired")?,
        finished_at: row.try_get("finished_at")?,
        artifact_id: row.try_get("artifact_id")?,
        artifact_version_id: row.try_get("artifact_version_id")?,
        content_digest: row.try_get("content_digest")?,
        size_bytes: row.try_get("size_bytes")?,
        redaction: row.try_get("redaction")?,
        stats: row.try_get("stats")?,
        proposal: row.try_get("proposal")?,
        skill_version_id: row.try_get("skill_version_id")?,
        abort_reason: row.try_get("abort_reason")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Recording {
        let now = Utc::now();
        Recording {
            id: "rec_01J8Z3K6F1N8VQ2X5W9YADP003".to_string(),
            tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9YADP003".to_string(),
            workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9YADP003".to_string(),
            owner_user_id: "usr_01J8Z3K6F1N8VQ2X5W9YADP003".to_string(),
            session_family_id: Some("fam_1".to_string()),
            epoch: 2,
            status: RecordingStatus::Recording,
            start_key: "k".to_string(),
            started_at: now,
            expires_at: now,
            expired: false,
            finished_at: None,
            artifact_id: None,
            artifact_version_id: None,
            content_digest: None,
            size_bytes: None,
            redaction: None,
            stats: None,
            proposal: None,
            skill_version_id: None,
            abort_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn caller(row: &Recording) -> RecordingCaller {
        RecordingCaller {
            tenant_id: row.tenant_id.clone(),
            user_id: row.owner_user_id.clone(),
            session_family_id: row.session_family_id.clone(),
            epoch: row.epoch,
            correlation_id: CorrelationId::generate(&mut UlidGenerator::new()),
        }
    }

    #[test]
    fn the_fence_names_the_first_part_that_fails() {
        let row = row();
        assert_eq!(violation(&row, &caller(&row)), None);
        let mut other_user = caller(&row);
        other_user.user_id = "usr_01J8Z3K6F1N8VQ2X5W9YOTHER1".to_string();
        assert_eq!(violation(&row, &other_user), Some("the call is from another user"));
        let mut other_tenant = caller(&row);
        other_tenant.tenant_id = "tn_01J8Z3K6F1N8VQ2X5W9YOTHER1".to_string();
        assert!(violation(&row, &other_tenant).is_some());
        let mut other_session = caller(&row);
        other_session.session_family_id = Some("fam_2".to_string());
        assert_eq!(violation(&row, &other_session), Some("the call is from another sign-in session"));
        let mut no_session = caller(&row);
        no_session.session_family_id = None;
        assert!(violation(&row, &no_session).is_some(), "a recording bound to a session needs one");
        let mut other_epoch = caller(&row);
        other_epoch.epoch = 3;
        assert_eq!(violation(&row, &other_epoch), Some("the call carries another epoch"));
        let mut header_scoped = row.clone();
        header_scoped.session_family_id = None;
        assert_eq!(violation(&header_scoped, &no_session), None, "header-scoped development has no session to bind");
    }

    #[test]
    fn status_spellings_round_trip() {
        for status in [
            RecordingStatus::Recording,
            RecordingStatus::Finished,
            RecordingStatus::Compiled,
            RecordingStatus::Submitted,
            RecordingStatus::Cancelled,
            RecordingStatus::Aborted,
        ] {
            assert_eq!(RecordingStatus::parse(status.as_str()).unwrap(), status);
        }
        assert!(RecordingStatus::parse("done").is_err());
    }
}
