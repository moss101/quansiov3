//! Metadata authority for artifacts, evidence and evidence bundles (DOMAIN.md §10).
//!
//! PostgreSQL is authoritative for identity, digests, grants and retention; object
//! storage holds only bytes (DOSSIER.md §9). Every store method takes a caller-owned
//! `&mut PgConnection` and sets the tenant context on it, so a caller can compose
//! several stores in one transaction exactly like `crates/server/src/runtime/`.
//!
//! Fail-closed rules implemented here:
//!
//! * bytes are addressed by sha256 and re-verified on every read;
//! * uploading the same version twice is idempotent (same identity, no rewrite);
//! * evidence rows are insert-only — a second capture is a new `evd_` identity;
//! * an artifact access without a grant is denied, and a read grant cannot write;
//! * deletion honours legal hold and retention and records the decision it made.

use chrono::{DateTime, Utc};
use quansio_core::{Digest, UlidGenerator};
use serde_json::Value;
use sqlx::postgres::{PgConnection, PgRow};
use sqlx::Row;

use crate::artifacts::model::{
    ArtifactGrant, ArtifactId, ArtifactKind, ArtifactVersionId, CapturedBy, Claim,
    DeletionDecision, DeletionRecord, EvidenceBundleId, EvidenceId, EvidenceKind, GrantLevel,
    Origin, ProducedBy, Retention, Source, Verification,
};
use crate::artifacts::object_store::{ObjectKey, ObjectStore, ObjectStoreError};
use crate::control::schema::{set_tenant_context_conn, SchemaError};

/// Errors from the artifact/evidence metadata authority.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// PostgreSQL rejected the operation.
    #[error("artifact store database: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant context could not be established.
    #[error("artifact store schema: {0}")]
    Schema(#[from] SchemaError),
    /// Object storage failed.
    #[error("artifact object storage: {0}")]
    ObjectStore(#[from] ObjectStoreError),
    /// A stored value could not be decoded into its canonical shape.
    #[error("decode {entity}: {detail}")]
    Decode {
        /// Entity being decoded.
        entity: &'static str,
        /// Failure detail.
        detail: String,
    },
    /// A caller-supplied value is not canonical.
    #[error("invalid {field}: {value}")]
    InvalidValue {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: String,
    },
    /// The requested entity does not exist for this tenant.
    #[error("{entity} not found: {id}")]
    NotFound {
        /// Entity kind.
        entity: &'static str,
        /// Requested identity.
        id: String,
    },
    /// The caller holds no grant (or an insufficient one) for the artifact.
    #[error("principal {principal} lacks {required} access to artifact {artifact_id}")]
    AccessDenied {
        /// Caller principal.
        principal: String,
        /// Artifact identity.
        artifact_id: String,
        /// Required level.
        required: &'static str,
    },
    /// Stored bytes do not hash to the recorded digest.
    #[error("{entity} {id} digest mismatch: expected {expected}, computed {actual}")]
    DigestMismatch {
        /// Entity kind.
        entity: &'static str,
        /// Entity identity.
        id: String,
        /// Recorded digest.
        expected: String,
        /// Digest of the bytes actually read.
        actual: String,
    },
    /// The artifact is tombstoned; its bytes are no longer served.
    #[error("artifact {0} is tombstoned")]
    Deleted(String),
    /// The evidence row carries no object bytes.
    #[error("evidence {0} has no object bytes")]
    NoBytes(String),
}

impl ArtifactError {
    fn encode(error: serde_json::Error) -> Self {
        Self::Decode {
            entity: "json",
            detail: error.to_string(),
        }
    }
}

/// An artifact aggregate as stored (DOMAIN.md §10.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    /// Artifact identity (`art_…`).
    pub id: ArtifactId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Human title.
    pub title: String,
    /// Role such as `report`, `source` or `attachment`.
    pub role: Option<String>,
    /// Origin of the artifact.
    pub origin: Origin,
    /// Current version, when a version exists.
    pub current_version_id: Option<ArtifactVersionId>,
    /// Per-principal grants.
    pub grants: Vec<ArtifactGrant>,
    /// Retention metadata.
    pub retention: Retention,
    /// Tombstone timestamp, when deleted.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// One immutable artifact version (DOMAIN.md §10.1).
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactVersion {
    /// Version identity (`artv_…`).
    pub id: ArtifactVersionId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning artifact.
    pub artifact_id: ArtifactId,
    /// Monotonic sequence within the artifact.
    pub seq: i32,
    /// SHA-256 digest of the bytes.
    pub content_digest: Digest,
    /// Byte length.
    pub size_bytes: i64,
    /// Media type.
    pub media_type: String,
    /// Tenant-prefixed object key.
    pub object_key: ObjectKey,
    /// Producing run/step/user.
    pub produced_by: ProducedBy,
    /// Previous version, when this version supersedes one.
    pub parent_version_id: Option<ArtifactVersionId>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// A request to create an artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct NewArtifact {
    /// Owning workspace.
    pub workspace_id: String,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Human title.
    pub title: String,
    /// Role such as `report`, `source` or `attachment`.
    pub role: Option<String>,
    /// Origin of the artifact.
    pub origin: Origin,
    /// Initial grants; the creating principal must appear with `write`.
    pub grants: Vec<ArtifactGrant>,
    /// Retention metadata.
    pub retention: Retention,
}

/// A request to add an immutable version.
#[derive(Debug, Clone, PartialEq)]
pub struct NewVersion {
    /// Exact bytes to store.
    pub bytes: Vec<u8>,
    /// Media type of the bytes.
    pub media_type: String,
    /// Optional digest the caller claims; a mismatch fails closed before any write.
    pub expected_digest: Option<Digest>,
    /// Producing run/step/user.
    pub produced_by: ProducedBy,
}

/// Result of adding a version: the stored version and whether it was a replay.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredVersion {
    /// Stored (existing or new) version.
    pub version: ArtifactVersion,
    /// `true` when identical bytes were already stored for this artifact.
    pub replayed: bool,
}

/// Outcome of a deletion request.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletionOutcome {
    /// What the deletion path decided.
    pub decision: DeletionDecision,
    /// Machine-readable reason.
    pub reason: String,
    /// Tombstone timestamp, for a tombstoned artifact.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Whether object bytes were removed.
    pub object_removed: bool,
}

/// A recorded evidence row (DOMAIN.md §10.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// Evidence identity (`evd_…`).
    pub id: EvidenceId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Producing run, when any.
    pub run_id: Option<String>,
    /// Producing step, when any.
    pub step_id: Option<String>,
    /// Related effect, when any.
    pub effect_id: Option<String>,
    /// Evidence kind.
    pub kind: EvidenceKind,
    /// SHA-256 digest of the evidence content.
    pub content_digest: Digest,
    /// Object key, when the evidence has bytes.
    pub object_key: Option<ObjectKey>,
    /// Bounded inline summary.
    pub inline_summary: Option<String>,
    /// Capture timestamp.
    pub captured_at: DateTime<Utc>,
    /// Capturing host.
    pub captured_by: CapturedBy,
    /// Whether redaction was applied before capture.
    pub redaction_applied: bool,
    /// Insert timestamp.
    pub created_at: DateTime<Utc>,
}

/// Content carried by a new evidence row.
#[derive(Debug, Clone, PartialEq)]
pub enum EvidenceContent {
    /// Bytes stored in object storage.
    Bytes {
        /// Exact bytes.
        bytes: Vec<u8>,
        /// Media type.
        media_type: String,
    },
    /// Digest-only evidence with no stored object.
    DigestOnly {
        /// Declared content digest.
        digest: Digest,
    },
}

/// A request to record evidence. Evidence is insert-only; there is no update path.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvidence {
    /// Owning workspace.
    pub workspace_id: String,
    /// Producing run, when any.
    pub run_id: Option<String>,
    /// Producing step, when any.
    pub step_id: Option<String>,
    /// Related effect, when any.
    pub effect_id: Option<String>,
    /// Evidence kind.
    pub kind: EvidenceKind,
    /// Evidence content.
    pub content: EvidenceContent,
    /// Bounded inline summary.
    pub inline_summary: Option<String>,
    /// Capture timestamp; defaults to the database clock when absent.
    pub captured_at: Option<DateTime<Utc>>,
    /// Capturing host.
    pub captured_by: CapturedBy,
    /// Whether redaction was applied before capture.
    pub redaction_applied: bool,
}

/// A recorded evidence bundle (DOMAIN.md §10.3, CAP-002 seam).
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceBundle {
    /// Bundle identity (`evb_…`).
    pub id: EvidenceBundleId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Producing run.
    pub run_id: String,
    /// Output artifact version, when the bundle backs one.
    pub artifact_version_id: Option<ArtifactVersionId>,
    /// Claims with their supporting evidence ids.
    pub claims: Vec<Claim>,
    /// Sources behind the claims.
    pub sources: Vec<Source>,
    /// Fraction of claims covered by evidence.
    pub coverage: Option<f64>,
    /// Verification status.
    pub verification: Verification,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// A request to record an evidence bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvidenceBundle {
    /// Producing run.
    pub run_id: String,
    /// Output artifact version, when any.
    pub artifact_version_id: Option<ArtifactVersionId>,
    /// Claims with their supporting evidence ids.
    pub claims: Vec<Claim>,
    /// Sources behind the claims.
    pub sources: Vec<Source>,
    /// Fraction of claims covered by evidence.
    pub coverage: Option<f64>,
    /// Verification status.
    pub verification: Verification,
}

/// Artifact metadata authority plus object-storage coordination.
pub struct ArtifactStore<'a> {
    objects: &'a dyn ObjectStore,
}

impl<'a> ArtifactStore<'a> {
    /// Build a store over the object-storage boundary.
    #[must_use]
    pub fn new(objects: &'a dyn ObjectStore) -> Self {
        Self { objects }
    }

    /// Create an artifact and its initial grants.
    ///
    /// The creating principal must hold `write` in `input.grants`, so authorization
    /// cannot be widened by creating an artifact nobody is allowed to use.
    ///
    /// # Errors
    /// Returns [`ArtifactError::AccessDenied`] when the creator has no write grant.
    pub async fn create(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        principal: &str,
        input: &NewArtifact,
    ) -> Result<Artifact, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let mut generator = UlidGenerator::new();
        let id = ArtifactId::generate(&mut generator);
        if !input
            .grants
            .iter()
            .any(|grant| grant.principal == principal && grant.level.satisfies(GrantLevel::Write))
        {
            return Err(ArtifactError::AccessDenied {
                principal: principal.to_string(),
                artifact_id: id.to_canonical(),
                required: GrantLevel::Write.as_str(),
            });
        }
        let origin = serde_json::to_value(&input.origin).map_err(ArtifactError::encode)?;
        let retention = serde_json::to_value(&input.retention).map_err(ArtifactError::encode)?;
        sqlx::query(
            "INSERT INTO artifacts (id, tenant_id, workspace_id, kind, title, role, origin, retention) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id.to_canonical())
        .bind(tenant_id)
        .bind(&input.workspace_id)
        .bind(input.kind.as_str())
        .bind(&input.title)
        .bind(&input.role)
        .bind(origin)
        .bind(retention)
        .execute(&mut *conn)
        .await?;
        for grant in &input.grants {
            insert_grant(&mut *conn, tenant_id, &id, grant).await?;
        }
        fetch_artifact(&mut *conn, tenant_id, &id).await
    }

    /// Load an artifact for a principal holding at least `read`.
    ///
    /// # Errors
    /// Returns [`ArtifactError::AccessDenied`] when no sufficient grant exists.
    pub async fn load(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        principal: &str,
        artifact_id: &ArtifactId,
    ) -> Result<Artifact, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let artifact = fetch_artifact(&mut *conn, tenant_id, artifact_id).await?;
        authorize(&artifact, principal, GrantLevel::Read)?;
        Ok(artifact)
    }

    /// Add an immutable version, or return the existing one for identical bytes.
    ///
    /// The object key embeds the sha256, so bytes are content-addressed and can never
    /// be overwritten in place; a second upload of the same bytes is a replay with the
    /// same `artv_` identity and no object rewrite.
    ///
    /// # Errors
    /// Returns [`ArtifactError::DigestMismatch`] when `expected_digest` disagrees with
    /// the supplied bytes, and [`ArtifactError::AccessDenied`] without a write grant.
    pub async fn add_version(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        principal: &str,
        artifact_id: &ArtifactId,
        input: &NewVersion,
    ) -> Result<StoredVersion, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let artifact = fetch_artifact(&mut *conn, tenant_id, artifact_id).await?;
        authorize(&artifact, principal, GrantLevel::Write)?;
        if artifact.deleted_at.is_some() {
            return Err(ArtifactError::Deleted(artifact_id.to_canonical()));
        }
        let digest = Digest::of(&input.bytes);
        if let Some(expected) = &input.expected_digest {
            if expected != &digest {
                return Err(ArtifactError::DigestMismatch {
                    entity: "artifact version",
                    id: artifact_id.to_canonical(),
                    expected: expected.to_string(),
                    actual: digest.to_string(),
                });
            }
        }
        if let Some(existing) =
            find_version_by_digest(&mut *conn, tenant_id, artifact_id, digest.as_str()).await?
        {
            return Ok(StoredVersion {
                version: existing,
                replayed: true,
            });
        }

        let mut generator = UlidGenerator::new();
        let version_id = ArtifactVersionId::generate(&mut generator);
        let object_key = ObjectKey::artifact_version(
            tenant_id,
            &artifact_id.to_canonical(),
            &version_id.to_canonical(),
            &digest,
        );
        self.objects
            .put(&object_key, &input.bytes, &input.media_type)
            .await?;

        let seq: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM artifact_versions \
             WHERE artifact_id = $1 AND tenant_id = $2",
        )
        .bind(artifact_id.to_canonical())
        .bind(tenant_id)
        .fetch_one(&mut *conn)
        .await?;

        sqlx::query(
            "INSERT INTO artifact_versions (id, tenant_id, artifact_id, seq, content_digest, \
             size_bytes, media_type, object_key, produced_by_run_id, produced_by_step_id, \
             produced_by_user_id, parent_version_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(version_id.to_canonical())
        .bind(tenant_id)
        .bind(artifact_id.to_canonical())
        .bind(seq)
        .bind(digest.as_str())
        .bind(input.bytes.len() as i64)
        .bind(&input.media_type)
        .bind(object_key.as_str())
        .bind(&input.produced_by.run_id)
        .bind(&input.produced_by.step_id)
        .bind(&input.produced_by.user_id)
        .bind(artifact.current_version_id.map(|id| id.to_canonical()))
        .execute(&mut *conn)
        .await?;

        sqlx::query(
            "UPDATE artifacts SET current_version_id = $3 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(artifact_id.to_canonical())
        .bind(tenant_id)
        .bind(version_id.to_canonical())
        .execute(&mut *conn)
        .await?;

        let version = fetch_version(&mut *conn, tenant_id, &version_id).await?;
        Ok(StoredVersion {
            version,
            replayed: false,
        })
    }

    /// Fetch a version's bytes and verify them against the recorded digest.
    ///
    /// # Errors
    /// Returns [`ArtifactError::DigestMismatch`] when the bytes do not hash to the
    /// recorded digest, [`ArtifactError::Deleted`] for a tombstoned artifact, or
    /// [`ArtifactError::AccessDenied`] without a read grant.
    pub async fn fetch_bytes(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        principal: &str,
        version_id: &ArtifactVersionId,
    ) -> Result<Vec<u8>, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let version = fetch_version(&mut *conn, tenant_id, version_id).await?;
        let artifact = fetch_artifact(&mut *conn, tenant_id, &version.artifact_id).await?;
        authorize(&artifact, principal, GrantLevel::Read)?;
        if artifact.deleted_at.is_some() {
            return Err(ArtifactError::Deleted(version.artifact_id.to_canonical()));
        }
        let bytes = self.objects.get(&version.object_key).await?;
        verify_digest(
            "artifact version",
            &version_id.to_canonical(),
            &version.content_digest,
            &bytes,
        )?;
        Ok(bytes)
    }

    /// Re-read a version's bytes and verify the digest without returning them.
    ///
    /// # Errors
    /// Same fail-closed conditions as [`ArtifactStore::fetch_bytes`].
    pub async fn verify_version(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        principal: &str,
        version_id: &ArtifactVersionId,
    ) -> Result<(), ArtifactError> {
        self.fetch_bytes(conn, tenant_id, principal, version_id)
            .await
            .map(|_| ())
    }

    /// Grant or replace one principal's access. The actor must already hold `write`.
    ///
    /// # Errors
    /// Returns [`ArtifactError::AccessDenied`] when the actor cannot write.
    pub async fn set_grant(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        principal: &str,
        artifact_id: &ArtifactId,
        grant: &ArtifactGrant,
    ) -> Result<(), ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let artifact = fetch_artifact(&mut *conn, tenant_id, artifact_id).await?;
        authorize(&artifact, principal, GrantLevel::Write)?;
        insert_grant(&mut *conn, tenant_id, artifact_id, grant).await
    }

    /// Request deletion, honouring legal hold and retention.
    ///
    /// A refused deletion records the reason and changes nothing; an allowed deletion
    /// tombstones the artifact metadata, records the reason and removes the version
    /// objects, so a removal is never silent.
    ///
    /// # Errors
    /// Returns [`ArtifactError::AccessDenied`] when the actor cannot write.
    pub async fn request_deletion(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        principal: &str,
        artifact_id: &ArtifactId,
    ) -> Result<DeletionOutcome, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let artifact = fetch_artifact(&mut *conn, tenant_id, artifact_id).await?;
        authorize(&artifact, principal, GrantLevel::Write)?;

        let now = Utc::now();
        if artifact.deleted_at.is_some() {
            let reason = artifact
                .retention
                .deletion
                .as_ref()
                .map_or_else(|| "already_tombstoned".to_string(), |d| d.reason.clone());
            return Ok(DeletionOutcome {
                decision: DeletionDecision::Tombstoned,
                reason,
                deleted_at: artifact.deleted_at,
                object_removed: false,
            });
        }

        let (decision, reason) = if artifact.retention.is_legal_hold() {
            (DeletionDecision::Refused, "legal_hold")
        } else if artifact.retention.is_retention_active(now) {
            (DeletionDecision::Refused, "retention_active")
        } else if artifact.retention.expires_at.is_some() {
            (DeletionDecision::Tombstoned, "retention_elapsed")
        } else {
            (DeletionDecision::Tombstoned, "retention_not_set")
        };

        let mut retention = artifact.retention.clone();
        retention.deletion = Some(DeletionRecord {
            decision,
            reason: reason.to_string(),
            recorded_at: now,
        });
        let retention = serde_json::to_value(&retention).map_err(ArtifactError::encode)?;

        let deleted_at = match decision {
            DeletionDecision::Refused => {
                sqlx::query("UPDATE artifacts SET retention = $3 WHERE id = $1 AND tenant_id = $2")
                    .bind(artifact_id.to_canonical())
                    .bind(tenant_id)
                    .bind(retention)
                    .execute(&mut *conn)
                    .await?;
                None
            }
            DeletionDecision::Tombstoned => {
                sqlx::query(
                    "UPDATE artifacts SET retention = $3, deleted_at = $4 \
                     WHERE id = $1 AND tenant_id = $2",
                )
                .bind(artifact_id.to_canonical())
                .bind(tenant_id)
                .bind(retention)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                Some(now)
            }
        };

        let mut object_removed = false;
        if decision == DeletionDecision::Tombstoned {
            let keys: Vec<String> = sqlx::query_scalar(
                "SELECT object_key FROM artifact_versions WHERE artifact_id = $1 AND tenant_id = $2",
            )
            .bind(artifact_id.to_canonical())
            .bind(tenant_id)
            .fetch_all(&mut *conn)
            .await?;
            for key in keys {
                let key = ObjectKey::parse(&key)?;
                match self.objects.delete(&key).await {
                    Ok(()) | Err(ObjectStoreError::NotFound(_)) => object_removed = true,
                    Err(error) => return Err(error.into()),
                }
            }
        }

        Ok(DeletionOutcome {
            decision,
            reason: reason.to_string(),
            deleted_at,
            object_removed,
        })
    }
}

/// Evidence metadata authority. Evidence rows are insert-only and never overwritten.
pub struct EvidenceStore<'a> {
    objects: &'a dyn ObjectStore,
}

impl<'a> EvidenceStore<'a> {
    /// Build a store over the object-storage boundary.
    #[must_use]
    pub fn new(objects: &'a dyn ObjectStore) -> Self {
        Self { objects }
    }

    /// Record one evidence row. Each call mints a new `evd_` identity.
    ///
    /// # Errors
    /// Returns [`ArtifactError::ObjectStore`] when bytes cannot be stored.
    pub async fn record(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        input: &NewEvidence,
    ) -> Result<Evidence, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let mut generator = UlidGenerator::new();
        let id = EvidenceId::generate(&mut generator);

        let (digest, object_key) = match &input.content {
            EvidenceContent::Bytes { bytes, media_type } => {
                let digest = Digest::of(bytes);
                let key = ObjectKey::evidence(tenant_id, &id.to_canonical(), &digest);
                self.objects.put(&key, bytes, media_type).await?;
                (digest, Some(key))
            }
            EvidenceContent::DigestOnly { digest } => (digest.clone(), None),
        };

        sqlx::query(
            "INSERT INTO evidence (id, tenant_id, workspace_id, run_id, step_id, effect_id, kind, \
             content_digest, object_key, inline_summary, captured_at, captured_by_kind, \
             captured_by_id, redaction_applied) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, COALESCE($11, now()), $12, $13, $14)",
        )
        .bind(id.to_canonical())
        .bind(tenant_id)
        .bind(&input.workspace_id)
        .bind(&input.run_id)
        .bind(&input.step_id)
        .bind(&input.effect_id)
        .bind(input.kind.as_str())
        .bind(digest.as_str())
        .bind(object_key.as_ref().map(ObjectKey::as_str))
        .bind(&input.inline_summary)
        .bind(input.captured_at)
        .bind(&input.captured_by.kind)
        .bind(&input.captured_by.id)
        .bind(input.redaction_applied)
        .execute(&mut *conn)
        .await?;

        fetch_evidence(&mut *conn, tenant_id, &id).await
    }

    /// Load one evidence row.
    ///
    /// # Errors
    /// Returns [`ArtifactError::NotFound`] when the row does not exist.
    pub async fn load(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        evidence_id: &EvidenceId,
    ) -> Result<Evidence, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        fetch_evidence(&mut *conn, tenant_id, evidence_id).await
    }

    /// Fetch evidence bytes and verify them against the recorded digest.
    ///
    /// # Errors
    /// Returns [`ArtifactError::DigestMismatch`] when bytes and digest disagree.
    pub async fn fetch_bytes(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        evidence_id: &EvidenceId,
    ) -> Result<Vec<u8>, ArtifactError> {
        let evidence = self.load(conn, tenant_id, evidence_id).await?;
        let key = evidence
            .object_key
            .ok_or_else(|| ArtifactError::NoBytes(evidence_id.to_canonical()))?;
        let bytes = self.objects.get(&key).await?;
        verify_digest(
            "evidence",
            &evidence_id.to_canonical(),
            &evidence.content_digest,
            &bytes,
        )?;
        Ok(bytes)
    }
}

/// Evidence-bundle authority (CAP-002 seam, DOMAIN.md §10.3).
pub struct EvidenceBundleStore;

impl EvidenceBundleStore {
    /// Record one evidence bundle.
    ///
    /// # Errors
    /// Returns [`ArtifactError::Database`] when the run does not exist.
    pub async fn record(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        input: &NewEvidenceBundle,
    ) -> Result<EvidenceBundle, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let mut generator = UlidGenerator::new();
        let id = EvidenceBundleId::generate(&mut generator);
        let claims = serde_json::to_value(&input.claims).map_err(ArtifactError::encode)?;
        let sources = serde_json::to_value(&input.sources).map_err(ArtifactError::encode)?;
        let verification =
            serde_json::to_value(&input.verification).map_err(ArtifactError::encode)?;
        sqlx::query(
            "INSERT INTO evidence_bundles (id, tenant_id, run_id, artifact_version_id, claims, \
             sources, coverage, verification) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id.to_canonical())
        .bind(tenant_id)
        .bind(&input.run_id)
        .bind(input.artifact_version_id.map(|id| id.to_canonical()))
        .bind(claims)
        .bind(sources)
        .bind(input.coverage)
        .bind(verification)
        .execute(&mut *conn)
        .await?;
        fetch_bundle(&mut *conn, tenant_id, &id).await
    }

    /// Load one evidence bundle.
    ///
    /// # Errors
    /// Returns [`ArtifactError::NotFound`] when the bundle does not exist.
    pub async fn load(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        bundle_id: &EvidenceBundleId,
    ) -> Result<EvidenceBundle, ArtifactError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        fetch_bundle(&mut *conn, tenant_id, bundle_id).await
    }
}

fn verify_digest(
    entity: &'static str,
    id: &str,
    expected: &Digest,
    bytes: &[u8],
) -> Result<(), ArtifactError> {
    let actual = Digest::of(bytes);
    if &actual == expected {
        Ok(())
    } else {
        Err(ArtifactError::DigestMismatch {
            entity,
            id: id.to_string(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        })
    }
}

fn authorize(
    artifact: &Artifact,
    principal: &str,
    required: GrantLevel,
) -> Result<(), ArtifactError> {
    let allowed = artifact
        .grants
        .iter()
        .any(|grant| grant.principal == principal && grant.level.satisfies(required));
    if allowed {
        Ok(())
    } else {
        Err(ArtifactError::AccessDenied {
            principal: principal.to_string(),
            artifact_id: artifact.id.to_canonical(),
            required: required.as_str(),
        })
    }
}

fn decode_error(entity: &'static str, error: sqlx::Error) -> ArtifactError {
    ArtifactError::Decode {
        entity,
        detail: error.to_string(),
    }
}

async fn insert_grant(
    conn: &mut PgConnection,
    tenant_id: &str,
    artifact_id: &ArtifactId,
    grant: &ArtifactGrant,
) -> Result<(), ArtifactError> {
    let row_id = format!("arg_{}", UlidGenerator::new().generate().to_base32());
    sqlx::query(
        "INSERT INTO artifact_grants (id, tenant_id, artifact_id, principal, level) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (artifact_id, principal) DO UPDATE SET level = EXCLUDED.level",
    )
    .bind(row_id)
    .bind(tenant_id)
    .bind(artifact_id.to_canonical())
    .bind(&grant.principal)
    .bind(grant.level.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

async fn fetch_artifact(
    conn: &mut PgConnection,
    tenant_id: &str,
    artifact_id: &ArtifactId,
) -> Result<Artifact, ArtifactError> {
    let id = artifact_id.to_canonical();
    let row = sqlx::query(
        "SELECT id, tenant_id, workspace_id, kind, title, role, origin, current_version_id, \
         retention, deleted_at, created_at FROM artifacts WHERE id = $1 AND tenant_id = $2",
    )
    .bind(&id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ArtifactError::NotFound {
        entity: "artifact",
        id: id.clone(),
    })?;
    let mut artifact = decode_artifact(&row)?;
    artifact.grants = fetch_grants(&mut *conn, tenant_id, artifact_id).await?;
    Ok(artifact)
}

async fn fetch_grants(
    conn: &mut PgConnection,
    tenant_id: &str,
    artifact_id: &ArtifactId,
) -> Result<Vec<ArtifactGrant>, ArtifactError> {
    let rows = sqlx::query(
        "SELECT principal, level FROM artifact_grants WHERE artifact_id = $1 AND tenant_id = $2 \
         ORDER BY principal",
    )
    .bind(artifact_id.to_canonical())
    .bind(tenant_id)
    .fetch_all(conn)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(ArtifactGrant {
                principal: row
                    .try_get("principal")
                    .map_err(|error| decode_error("artifact grant", error))?,
                level: decode_grant_level(
                    &row.try_get::<String, _>("level")
                        .map_err(|error| decode_error("artifact grant", error))?,
                )?,
            })
        })
        .collect()
}

fn decode_grant_level(value: &str) -> Result<GrantLevel, ArtifactError> {
    match value {
        "read" => Ok(GrantLevel::Read),
        "write" => Ok(GrantLevel::Write),
        other => Err(ArtifactError::InvalidValue {
            field: "grant level",
            value: other.to_string(),
        }),
    }
}

async fn fetch_version(
    conn: &mut PgConnection,
    tenant_id: &str,
    version_id: &ArtifactVersionId,
) -> Result<ArtifactVersion, ArtifactError> {
    let id = version_id.to_canonical();
    let row = sqlx::query(
        "SELECT id, tenant_id, artifact_id, seq, content_digest, size_bytes, media_type, \
         object_key, produced_by_run_id, produced_by_step_id, produced_by_user_id, \
         parent_version_id, created_at FROM artifact_versions WHERE id = $1 AND tenant_id = $2",
    )
    .bind(&id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ArtifactError::NotFound {
        entity: "artifact version",
        id: id.clone(),
    })?;
    decode_version(&row)
}

async fn find_version_by_digest(
    conn: &mut PgConnection,
    tenant_id: &str,
    artifact_id: &ArtifactId,
    digest: &str,
) -> Result<Option<ArtifactVersion>, ArtifactError> {
    let row = sqlx::query(
        "SELECT id, tenant_id, artifact_id, seq, content_digest, size_bytes, media_type, \
         object_key, produced_by_run_id, produced_by_step_id, produced_by_user_id, \
         parent_version_id, created_at FROM artifact_versions \
         WHERE artifact_id = $1 AND tenant_id = $2 AND content_digest = $3 \
         ORDER BY seq LIMIT 1",
    )
    .bind(artifact_id.to_canonical())
    .bind(tenant_id)
    .bind(digest)
    .fetch_optional(conn)
    .await?;
    row.map(|row| decode_version(&row)).transpose()
}

async fn fetch_evidence(
    conn: &mut PgConnection,
    tenant_id: &str,
    evidence_id: &EvidenceId,
) -> Result<Evidence, ArtifactError> {
    let id = evidence_id.to_canonical();
    let row = sqlx::query(
        "SELECT id, tenant_id, workspace_id, run_id, step_id, effect_id, kind, content_digest, \
         object_key, inline_summary, captured_at, captured_by_kind, captured_by_id, \
         redaction_applied, created_at FROM evidence WHERE id = $1 AND tenant_id = $2",
    )
    .bind(&id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ArtifactError::NotFound {
        entity: "evidence",
        id: id.clone(),
    })?;
    decode_evidence(&row)
}

async fn fetch_bundle(
    conn: &mut PgConnection,
    tenant_id: &str,
    bundle_id: &EvidenceBundleId,
) -> Result<EvidenceBundle, ArtifactError> {
    let id = bundle_id.to_canonical();
    let row = sqlx::query(
        "SELECT id, tenant_id, run_id, artifact_version_id, claims, sources, coverage, \
         verification, created_at FROM evidence_bundles WHERE id = $1 AND tenant_id = $2",
    )
    .bind(&id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ArtifactError::NotFound {
        entity: "evidence bundle",
        id: id.clone(),
    })?;
    decode_bundle(&row)
}

fn decode_artifact(row: &PgRow) -> Result<Artifact, ArtifactError> {
    let origin: Value = row
        .try_get("origin")
        .map_err(|e| decode_error("artifact", e))?;
    let retention: Value = row
        .try_get("retention")
        .map_err(|e| decode_error("artifact", e))?;
    let current: Option<String> = row
        .try_get("current_version_id")
        .map_err(|e| decode_error("artifact", e))?;
    let deleted_at: Option<DateTime<Utc>> = row
        .try_get("deleted_at")
        .map_err(|e| decode_error("artifact", e))?;
    Ok(Artifact {
        id: parse_id(
            "artifact",
            &row.try_get::<String, _>("id")
                .map_err(|e| decode_error("artifact", e))?,
        )?,
        tenant_id: row
            .try_get("tenant_id")
            .map_err(|e| decode_error("artifact", e))?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(|e| decode_error("artifact", e))?,
        kind: decode_artifact_kind(
            &row.try_get::<String, _>("kind")
                .map_err(|e| decode_error("artifact", e))?,
        )?,
        title: row
            .try_get("title")
            .map_err(|e| decode_error("artifact", e))?,
        role: row
            .try_get("role")
            .map_err(|e| decode_error("artifact", e))?,
        origin: serde_json::from_value(origin).map_err(ArtifactError::encode)?,
        current_version_id: current
            .map(|value| parse_id("artifact version", &value))
            .transpose()?,
        grants: Vec::new(),
        retention: serde_json::from_value(retention).map_err(ArtifactError::encode)?,
        deleted_at,
        created_at: row
            .try_get("created_at")
            .map_err(|e| decode_error("artifact", e))?,
    })
}

fn decode_version(row: &PgRow) -> Result<ArtifactVersion, ArtifactError> {
    let digest: String = row
        .try_get("content_digest")
        .map_err(|e| decode_error("artifact version", e))?;
    let parent: Option<String> = row
        .try_get("parent_version_id")
        .map_err(|e| decode_error("artifact version", e))?;
    let object_key: String = row
        .try_get("object_key")
        .map_err(|e| decode_error("artifact version", e))?;
    Ok(ArtifactVersion {
        id: parse_id(
            "artifact version",
            &row.try_get::<String, _>("id")
                .map_err(|e| decode_error("artifact version", e))?,
        )?,
        tenant_id: row
            .try_get("tenant_id")
            .map_err(|e| decode_error("artifact version", e))?,
        artifact_id: parse_id(
            "artifact",
            &row.try_get::<String, _>("artifact_id")
                .map_err(|e| decode_error("artifact version", e))?,
        )?,
        seq: row
            .try_get("seq")
            .map_err(|e| decode_error("artifact version", e))?,
        content_digest: parse_digest(&digest)?,
        size_bytes: row
            .try_get("size_bytes")
            .map_err(|e| decode_error("artifact version", e))?,
        media_type: row
            .try_get("media_type")
            .map_err(|e| decode_error("artifact version", e))?,
        object_key: ObjectKey::parse(&object_key)?,
        produced_by: ProducedBy {
            run_id: row
                .try_get("produced_by_run_id")
                .map_err(|e| decode_error("artifact version", e))?,
            step_id: row
                .try_get("produced_by_step_id")
                .map_err(|e| decode_error("artifact version", e))?,
            user_id: row
                .try_get("produced_by_user_id")
                .map_err(|e| decode_error("artifact version", e))?,
        },
        parent_version_id: parent
            .map(|value| parse_id("artifact version", &value))
            .transpose()?,
        created_at: row
            .try_get("created_at")
            .map_err(|e| decode_error("artifact version", e))?,
    })
}

fn decode_evidence(row: &PgRow) -> Result<Evidence, ArtifactError> {
    let digest: String = row
        .try_get("content_digest")
        .map_err(|e| decode_error("evidence", e))?;
    let object_key: Option<String> = row
        .try_get("object_key")
        .map_err(|e| decode_error("evidence", e))?;
    Ok(Evidence {
        id: parse_id(
            "evidence",
            &row.try_get::<String, _>("id")
                .map_err(|e| decode_error("evidence", e))?,
        )?,
        tenant_id: row
            .try_get("tenant_id")
            .map_err(|e| decode_error("evidence", e))?,
        workspace_id: row
            .try_get("workspace_id")
            .map_err(|e| decode_error("evidence", e))?,
        run_id: row
            .try_get("run_id")
            .map_err(|e| decode_error("evidence", e))?,
        step_id: row
            .try_get("step_id")
            .map_err(|e| decode_error("evidence", e))?,
        effect_id: row
            .try_get("effect_id")
            .map_err(|e| decode_error("evidence", e))?,
        kind: decode_evidence_kind(
            &row.try_get::<String, _>("kind")
                .map_err(|e| decode_error("evidence", e))?,
        )?,
        content_digest: parse_digest(&digest)?,
        object_key: object_key
            .map(|value| ObjectKey::parse(&value))
            .transpose()?,
        inline_summary: row
            .try_get("inline_summary")
            .map_err(|e| decode_error("evidence", e))?,
        captured_at: row
            .try_get("captured_at")
            .map_err(|e| decode_error("evidence", e))?,
        captured_by: CapturedBy {
            kind: row
                .try_get("captured_by_kind")
                .map_err(|e| decode_error("evidence", e))?,
            id: row
                .try_get("captured_by_id")
                .map_err(|e| decode_error("evidence", e))?,
        },
        redaction_applied: row
            .try_get("redaction_applied")
            .map_err(|e| decode_error("evidence", e))?,
        created_at: row
            .try_get("created_at")
            .map_err(|e| decode_error("evidence", e))?,
    })
}

fn decode_bundle(row: &PgRow) -> Result<EvidenceBundle, ArtifactError> {
    let claims: Value = row
        .try_get("claims")
        .map_err(|e| decode_error("evidence bundle", e))?;
    let sources: Value = row
        .try_get("sources")
        .map_err(|e| decode_error("evidence bundle", e))?;
    let verification: Value = row
        .try_get("verification")
        .map_err(|e| decode_error("evidence bundle", e))?;
    let artifact_version: Option<String> = row
        .try_get("artifact_version_id")
        .map_err(|e| decode_error("evidence bundle", e))?;
    Ok(EvidenceBundle {
        id: parse_id(
            "evidence bundle",
            &row.try_get::<String, _>("id")
                .map_err(|e| decode_error("evidence bundle", e))?,
        )?,
        tenant_id: row
            .try_get("tenant_id")
            .map_err(|e| decode_error("evidence bundle", e))?,
        run_id: row
            .try_get("run_id")
            .map_err(|e| decode_error("evidence bundle", e))?,
        artifact_version_id: artifact_version
            .map(|value| parse_id("artifact version", &value))
            .transpose()?,
        claims: serde_json::from_value(claims).map_err(ArtifactError::encode)?,
        sources: serde_json::from_value(sources).map_err(ArtifactError::encode)?,
        coverage: row
            .try_get("coverage")
            .map_err(|e| decode_error("evidence bundle", e))?,
        verification: serde_json::from_value(verification).map_err(ArtifactError::encode)?,
        created_at: row
            .try_get("created_at")
            .map_err(|e| decode_error("evidence bundle", e))?,
    })
}

fn parse_id<T: std::str::FromStr<Err = quansio_core::CoreError>>(
    entity: &'static str,
    value: &str,
) -> Result<T, ArtifactError> {
    value
        .parse()
        .map_err(|error: quansio_core::CoreError| ArtifactError::Decode {
            entity,
            detail: error.to_string(),
        })
}

fn parse_digest(value: &str) -> Result<Digest, ArtifactError> {
    value
        .parse()
        .map_err(|error: quansio_core::CoreError| ArtifactError::Decode {
            entity: "digest",
            detail: error.to_string(),
        })
}

fn decode_artifact_kind(value: &str) -> Result<ArtifactKind, ArtifactError> {
    let kind = match value {
        "document" => ArtifactKind::Document,
        "spreadsheet" => ArtifactKind::Spreadsheet,
        "presentation" => ArtifactKind::Presentation,
        "code" => ArtifactKind::Code,
        "data" => ArtifactKind::Data,
        "image" => ArtifactKind::Image,
        "audio" => ArtifactKind::Audio,
        "video" => ArtifactKind::Video,
        "archive" => ArtifactKind::Archive,
        "other" => ArtifactKind::Other,
        other => {
            return Err(ArtifactError::InvalidValue {
                field: "artifact kind",
                value: other.to_string(),
            })
        }
    };
    Ok(kind)
}

fn decode_evidence_kind(value: &str) -> Result<EvidenceKind, ArtifactError> {
    let kind = match value {
        "tool_output" => EvidenceKind::ToolOutput,
        "screenshot" => EvidenceKind::Screenshot,
        "dom_snapshot" => EvidenceKind::DomSnapshot,
        "log" => EvidenceKind::Log,
        "test_result" => EvidenceKind::TestResult,
        "http_exchange" => EvidenceKind::HttpExchange,
        "diff" => EvidenceKind::Diff,
        "digest" => EvidenceKind::Digest,
        "external_ref" => EvidenceKind::ExternalRef,
        other => {
            return Err(ArtifactError::InvalidValue {
                field: "evidence kind",
                value: other.to_string(),
            })
        }
    };
    Ok(kind)
}
