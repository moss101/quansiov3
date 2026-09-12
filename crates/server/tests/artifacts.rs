//! Artifact and evidence storage tests (CORE-007).
//!
//! Metadata lives in PostgreSQL and bytes live in S3-compatible object storage. Every
//! test creates and drops its own scratch database, so a development database is never
//! touched. The round-trip, multipart and digest-corruption tests talk to the real
//! MinIO of the local development stack; the metadata-only tests use the narrow
//! [`ObjectStore`] trait with a deterministic in-process double, which is exactly why
//! the trait exists. The object-store double is never release evidence: whenever the
//! bucket is required the test fails rather than skips.
//!
//! Environment variables read:
//!
//! * `QUANSIO_TEST_POSTGRES_URL` — superuser DSN for the scratch databases, for example
//!   `postgres://quansio:…@127.0.0.1:55440/quansio`. When it is absent the tests print
//!   an explicit `BLOCKED_EXTERNAL` marker and return, because the local development
//!   stack (`scripts/dev/up`) is not running.
//! * `QUANSIO_TEST_MINIO_ENDPOINT` — default `http://127.0.0.1:59010`
//! * `QUANSIO_TEST_MINIO_REGION` — default `us-east-1`
//! * `QUANSIO_TEST_MINIO_BUCKET` — default `quansio-dev`
//! * `QUANSIO_TEST_MINIO_ACCESS_KEY` — default `quansio-dev`
//! * `QUANSIO_TEST_MINIO_SECRET_KEY` — default `quansio-dev-only` (documented dev-only
//!   default from `scripts/dev/_common.sh`; never a production credential)
//! * `QUANSIO_TEST_MINIO_SESSION_TOKEN` — optional
//! * `QUANSIO_TEST_MINIO_MULTIPART_THRESHOLD_BYTES` — default 8 MiB
//! * `QUANSIO_TEST_MINIO_PART_SIZE_BYTES` — default 8 MiB

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use quansio_core::Digest;
use quansio_server::artifacts::{
    ArtifactError, ArtifactGrant, ArtifactKind, ArtifactStore, CapturedBy, Claim, DeletionDecision,
    EvidenceBundleStore, EvidenceContent, EvidenceKind, EvidenceStore, GrantLevel, NewArtifact,
    NewEvidence, NewEvidenceBundle, NewVersion, ObjectKey, ObjectStore, ObjectStoreError, Origin,
    OriginKind, ProducedBy, Retention, S3Config, S3ObjectStore, Source, Verification,
};
use quansio_server::control::schema;
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const OTHER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const AGENT_THREAD: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const RUN: &str = "run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

/// Deterministic object-store double used only for metadata-logic tests.
#[derive(Default)]
struct MemoryStore {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    puts: AtomicUsize,
}

impl MemoryStore {
    fn put_count(&self) -> usize {
        self.puts.load(Ordering::SeqCst)
    }

    fn contains(&self, key: &ObjectKey) -> bool {
        self.objects
            .lock()
            .expect("lock")
            .contains_key(key.as_str())
    }
}

#[async_trait]
impl ObjectStore for MemoryStore {
    async fn put(
        &self,
        key: &ObjectKey,
        bytes: &[u8],
        _content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.objects
            .lock()
            .expect("lock")
            .insert(key.as_str().to_string(), bytes.to_vec());
        Ok(())
    }

    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, ObjectStoreError> {
        self.objects
            .lock()
            .expect("lock")
            .get(key.as_str())
            .cloned()
            .ok_or_else(|| ObjectStoreError::NotFound(key.as_str().to_string()))
    }

    async fn exists(&self, key: &ObjectKey) -> Result<bool, ObjectStoreError> {
        Ok(self.contains(key))
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), ObjectStoreError> {
        self.objects.lock().expect("lock").remove(key.as_str());
        Ok(())
    }
}

async fn scratch(prefix: &str) -> Option<(PgPool, String)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    Some((pool, name))
}

fn minio() -> S3ObjectStore {
    S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration")
}

fn new_artifact(grants: Vec<ArtifactGrant>, retention: Retention) -> NewArtifact {
    NewArtifact {
        workspace_id: WORKSPACE.to_string(),
        kind: ArtifactKind::Document,
        title: "quarterly report".to_string(),
        role: Some("report".to_string()),
        origin: Origin {
            kind: OriginKind::Upload,
            reference: "upload-1".to_string(),
        },
        grants,
        retention,
    }
}

fn write_grant(principal: &str) -> ArtifactGrant {
    ArtifactGrant {
        principal: principal.to_string(),
        level: GrantLevel::Write,
    }
}

fn new_version(bytes: &[u8]) -> NewVersion {
    NewVersion {
        bytes: bytes.to_vec(),
        media_type: "text/plain".to_string(),
        expected_digest: None,
        produced_by: ProducedBy::default(),
    }
}

async fn seed_run(pool: &PgPool) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind) \
         VALUES ($1, $2, $3, 'worker')",
    )
    .bind(AGENT_THREAD)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("insert agent thread");
    sqlx::query(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, trigger_kind) \
         VALUES ($1, $2, $3, $4, $5, 'manual')",
    )
    .bind(RUN)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(WORK_NODE)
    .bind(AGENT_THREAD)
    .execute(&mut *tx)
    .await
    .expect("insert run");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn artifact_round_trip_verifies_the_recorded_digest_against_minio() {
    let Some((pool, name)) = scratch("artroundtrip").await else {
        blocked_marker();
        return;
    };
    let objects = minio();
    let store = ArtifactStore::new(&objects);
    let mut conn = pool.acquire().await.expect("acquire");

    let artifact = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(vec![write_grant(USER)], Retention::default()),
        )
        .await
        .expect("create artifact");

    let payload = b"quarterly report, version one";
    let stored = store
        .add_version(&mut conn, TENANT, USER, &artifact.id, &new_version(payload))
        .await
        .expect("upload version");
    assert!(!stored.replayed, "first upload is not a replay");
    assert_eq!(stored.version.seq, 1);
    assert_eq!(stored.version.size_bytes, payload.len() as i64);
    assert_eq!(
        stored.version.content_digest,
        Digest::of(payload),
        "recorded digest must be the sha256 of the uploaded bytes"
    );
    assert_eq!(
        stored.version.object_key.as_str(),
        format!(
            "tenants/{TENANT}/artifacts/{}/versions/{}/{}",
            artifact.id,
            stored.version.id,
            Digest::of(payload)
        ),
        "object keys are tenant-prefixed and content-addressed"
    );

    let fetched = store
        .fetch_bytes(&mut conn, TENANT, USER, &stored.version.id)
        .await
        .expect("fetch and verify");
    assert_eq!(fetched, payload);
    assert_eq!(Digest::of(&fetched), stored.version.content_digest);

    // Uploading the identical bytes again is idempotent: same identity, no rewrite.
    let replayed = store
        .add_version(&mut conn, TENANT, USER, &artifact.id, &new_version(payload))
        .await
        .expect("replay upload");
    assert!(replayed.replayed, "identical bytes must be a replay");
    assert_eq!(replayed.version.id, stored.version.id);

    let loaded = store
        .load(&mut conn, TENANT, USER, &artifact.id)
        .await
        .expect("load artifact");
    assert_eq!(loaded.current_version_id, Some(stored.version.id));

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn multipart_attachment_round_trips_against_minio() {
    let Some((pool, name)) = scratch("artmultipart").await else {
        blocked_marker();
        return;
    };
    let mut config = S3Config::from_env().expect("boundary configuration");
    config.multipart_threshold = 1024 * 1024;
    let objects = S3ObjectStore::new(config).expect("client");
    let store = ArtifactStore::new(&objects);
    let mut conn = pool.acquire().await.expect("acquire");

    let artifact = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(vec![write_grant(USER)], Retention::default()),
        )
        .await
        .expect("create artifact");

    // 9 MiB with an 8 MiB part size forces two real multipart parts.
    let payload: Vec<u8> = (0..9 * 1024 * 1024).map(|index| index as u8).collect();
    let stored = store
        .add_version(
            &mut conn,
            TENANT,
            USER,
            &artifact.id,
            &new_version(&payload),
        )
        .await
        .expect("multipart upload");
    assert_eq!(stored.version.size_bytes, payload.len() as i64);

    let fetched = store
        .fetch_bytes(&mut conn, TENANT, USER, &stored.version.id)
        .await
        .expect("fetch multipart object");
    assert_eq!(fetched.len(), payload.len());
    assert_eq!(Digest::of(&fetched), stored.version.content_digest);

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn digest_corruption_is_rejected_against_minio() {
    let Some((pool, name)) = scratch("artcorrupt").await else {
        blocked_marker();
        return;
    };
    let objects = minio();
    let store = ArtifactStore::new(&objects);
    let mut conn = pool.acquire().await.expect("acquire");

    let artifact = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(vec![write_grant(USER)], Retention::default()),
        )
        .await
        .expect("create artifact");
    let stored = store
        .add_version(
            &mut conn,
            TENANT,
            USER,
            &artifact.id,
            &new_version(b"original bytes"),
        )
        .await
        .expect("upload version");

    // Tamper with the stored bytes behind the metadata authority's back.
    objects
        .put(&stored.version.object_key, b"tampered bytes", "text/plain")
        .await
        .expect("simulate bucket tampering");
    let error = store
        .fetch_bytes(&mut conn, TENANT, USER, &stored.version.id)
        .await
        .expect_err("tampered bytes must be rejected");
    assert!(
        matches!(error, ArtifactError::DigestMismatch { .. }),
        "expected a typed digest mismatch, got {error}"
    );

    // Restore the bytes and corrupt the recorded digest instead.
    objects
        .put(&stored.version.object_key, b"original bytes", "text/plain")
        .await
        .expect("restore bytes");
    let wrong = Digest::of(b"not the stored bytes");
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "UPDATE artifact_versions SET content_digest = $1 WHERE id = $2 AND tenant_id = $3",
    )
    .bind(wrong.as_str())
    .bind(stored.version.id.to_canonical())
    .bind(TENANT)
    .execute(&mut *tx)
    .await
    .expect("corrupt metadata");
    tx.commit().await.expect("commit");

    let error = store
        .verify_version(&mut conn, TENANT, USER, &stored.version.id)
        .await
        .expect_err("tampered metadata must be rejected");
    assert!(
        matches!(error, ArtifactError::DigestMismatch { .. }),
        "expected a typed digest mismatch, got {error}"
    );

    // A caller-declared digest that disagrees with the bytes fails before any write.
    let before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artifact_versions WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_one(&mut *conn)
            .await
            .expect("count");
    let mut mismatched = new_version(b"a different payload");
    mismatched.expected_digest = Some(Digest::of(b"some other payload"));
    let error = store
        .add_version(&mut conn, TENANT, USER, &artifact.id, &mismatched)
        .await
        .expect_err("declared digest mismatch must fail closed");
    assert!(matches!(error, ArtifactError::DigestMismatch { .. }));
    let after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artifact_versions WHERE tenant_id = $1")
            .bind(TENANT)
            .fetch_one(&mut *conn)
            .await
            .expect("count");
    assert_eq!(before, after, "a rejected upload must not store metadata");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn new_bytes_get_a_new_version_identity() {
    let Some((pool, name)) = scratch("artidentity").await else {
        blocked_marker();
        return;
    };
    let objects = MemoryStore::default();
    let store = ArtifactStore::new(&objects);
    let mut conn = pool.acquire().await.expect("acquire");

    let artifact = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(vec![write_grant(USER)], Retention::default()),
        )
        .await
        .expect("create artifact");
    let first = store
        .add_version(&mut conn, TENANT, USER, &artifact.id, &new_version(b"v1"))
        .await
        .expect("v1");
    let second = store
        .add_version(&mut conn, TENANT, USER, &artifact.id, &new_version(b"v2"))
        .await
        .expect("v2");

    assert_ne!(
        first.version.id, second.version.id,
        "a new version is a new identity"
    );
    assert_eq!(first.version.seq, 1);
    assert_eq!(second.version.seq, 2);
    assert_eq!(
        second.version.parent_version_id,
        Some(first.version.id),
        "a new version records its parent"
    );
    assert_ne!(
        second.version.object_key, first.version.object_key,
        "immutable bytes per version live under distinct keys"
    );
    assert_eq!(
        objects.put_count(),
        2,
        "each distinct payload is written once"
    );

    let loaded = store
        .load(&mut conn, TENANT, USER, &artifact.id)
        .await
        .expect("load");
    assert_eq!(loaded.current_version_id, Some(second.version.id));

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn evidence_is_insert_only_and_never_overwritten() {
    let Some((pool, name)) = scratch("evdimmutable").await else {
        blocked_marker();
        return;
    };
    let objects = MemoryStore::default();
    let store = EvidenceStore::new(&objects);
    let captured = NewEvidence {
        workspace_id: WORKSPACE.to_string(),
        run_id: None,
        step_id: None,
        effect_id: None,
        kind: EvidenceKind::ToolOutput,
        content: EvidenceContent::Bytes {
            bytes: b"tool output".to_vec(),
            media_type: "text/plain".to_string(),
        },
        inline_summary: Some("bounded summary".to_string()),
        captured_at: None,
        captured_by: CapturedBy {
            kind: "run".to_string(),
            id: RUN.to_string(),
        },
        redaction_applied: false,
    };
    let mut conn = pool.acquire().await.expect("acquire");

    let first = store
        .record(&mut conn, TENANT, &captured)
        .await
        .expect("first");
    let second = store
        .record(&mut conn, TENANT, &captured)
        .await
        .expect("second");
    assert_ne!(
        first.id, second.id,
        "a second capture must create a new evidence identity"
    );

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM evidence WHERE tenant_id = $1")
        .bind(TENANT)
        .fetch_one(&mut *conn)
        .await
        .expect("count");
    assert_eq!(count, 2, "both immutable evidence rows are retained");

    let fetched = store
        .fetch_bytes(&mut conn, TENANT, &first.id)
        .await
        .expect("fetch evidence");
    assert_eq!(fetched, b"tool output");
    assert_eq!(first.content_digest, Digest::of(&fetched));

    // Tampering with evidence bytes is detected on read.
    let key = first
        .object_key
        .clone()
        .expect("evidence has an object key");
    objects
        .put(&key, b"rewritten", "text/plain")
        .await
        .expect("simulate tampering");
    let error = store
        .fetch_bytes(&mut conn, TENANT, &first.id)
        .await
        .expect_err("tampered evidence must be rejected");
    assert!(matches!(error, ArtifactError::DigestMismatch { .. }));

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn grant_scope_denies_ungranted_callers_and_read_cannot_write() {
    let Some((pool, name)) = scratch("artgrants").await else {
        blocked_marker();
        return;
    };
    let objects = MemoryStore::default();
    let store = ArtifactStore::new(&objects);
    let mut conn = pool.acquire().await.expect("acquire");

    // Creating an artifact the creator cannot use is refused.
    let error = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(
                vec![ArtifactGrant {
                    principal: OTHER.to_string(),
                    level: GrantLevel::Write,
                }],
                Retention::default(),
            ),
        )
        .await
        .expect_err("creator without a write grant must be denied");
    assert!(matches!(error, ArtifactError::AccessDenied { .. }));

    let artifact = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(vec![write_grant(USER)], Retention::default()),
        )
        .await
        .expect("create artifact");

    // No grant: denied.
    let error = store
        .load(&mut conn, TENANT, OTHER, &artifact.id)
        .await
        .expect_err("an ungranted caller must be denied");
    assert!(matches!(error, ArtifactError::AccessDenied { .. }));

    // A read grant cannot write.
    store
        .set_grant(
            &mut conn,
            TENANT,
            USER,
            &artifact.id,
            &ArtifactGrant {
                principal: OTHER.to_string(),
                level: GrantLevel::Read,
            },
        )
        .await
        .expect("grant read");
    store
        .load(&mut conn, TENANT, OTHER, &artifact.id)
        .await
        .expect("read grant allows loading");
    let error = store
        .add_version(
            &mut conn,
            TENANT,
            OTHER,
            &artifact.id,
            &new_version(b"writes"),
        )
        .await
        .expect_err("a read grant must not allow writes");
    assert!(matches!(error, ArtifactError::AccessDenied { .. }));

    // A read grant cannot widen access either.
    let error = store
        .set_grant(&mut conn, TENANT, OTHER, &artifact.id, &write_grant(OTHER))
        .await
        .expect_err("a read grant must not grant");
    assert!(matches!(error, ArtifactError::AccessDenied { .. }));

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn legal_hold_and_retention_are_recorded_not_silently_deleted() {
    let Some((pool, name)) = scratch("artretention").await else {
        blocked_marker();
        return;
    };
    let objects = MemoryStore::default();
    let store = ArtifactStore::new(&objects);
    let mut conn = pool.acquire().await.expect("acquire");

    // Legal hold: deletion refused and the reason recorded; bytes stay.
    let held = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(vec![write_grant(USER)], Retention::legal_hold()),
        )
        .await
        .expect("create held artifact");
    let held_version = store
        .add_version(
            &mut conn,
            TENANT,
            USER,
            &held.id,
            &new_version(b"under hold"),
        )
        .await
        .expect("held version");
    let outcome = store
        .request_deletion(&mut conn, TENANT, USER, &held.id)
        .await
        .expect("deletion decision");
    assert_eq!(outcome.decision, DeletionDecision::Refused);
    assert_eq!(outcome.reason, "legal_hold");
    assert!(outcome.deleted_at.is_none());
    assert!(!outcome.object_removed);
    assert!(objects.contains(&held_version.version.object_key));
    let reloaded = store
        .load(&mut conn, TENANT, USER, &held.id)
        .await
        .expect("load");
    assert!(
        reloaded.deleted_at.is_none(),
        "legal hold must not tombstone"
    );
    let recorded = reloaded
        .retention
        .deletion
        .expect("deletion reason recorded");
    assert_eq!(recorded.decision, DeletionDecision::Refused);
    assert_eq!(recorded.reason, "legal_hold");

    // Active retention: refused with the retention reason.
    let active = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(
                vec![write_grant(USER)],
                Retention::expiring_at(chrono::Utc::now() + chrono::Duration::hours(1)),
            ),
        )
        .await
        .expect("create active retention artifact");
    let outcome = store
        .request_deletion(&mut conn, TENANT, USER, &active.id)
        .await
        .expect("deletion decision");
    assert_eq!(outcome.decision, DeletionDecision::Refused);
    assert_eq!(outcome.reason, "retention_active");

    // Expired retention: tombstoned with a recorded reason, never silent.
    let expired = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &new_artifact(
                vec![write_grant(USER)],
                Retention::expiring_at(chrono::Utc::now() - chrono::Duration::hours(1)),
            ),
        )
        .await
        .expect("create expired artifact");
    let expired_version = store
        .add_version(
            &mut conn,
            TENANT,
            USER,
            &expired.id,
            &new_version(b"expired bytes"),
        )
        .await
        .expect("expired version");
    let outcome = store
        .request_deletion(&mut conn, TENANT, USER, &expired.id)
        .await
        .expect("deletion decision");
    assert_eq!(outcome.decision, DeletionDecision::Tombstoned);
    assert_eq!(outcome.reason, "retention_elapsed");
    assert!(outcome.deleted_at.is_some());
    assert!(outcome.object_removed);
    assert!(!objects.contains(&expired_version.version.object_key));
    // The metadata tombstone survives, and it names the reason.
    let row: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM artifacts WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NOT NULL",
    )
    .bind(expired.id.to_canonical())
    .bind(TENANT)
    .fetch_one(&mut *conn)
    .await
    .expect("tombstone row");
    assert_eq!(row, 1, "tombstone metadata is never removed");
    let reloaded = store
        .load(&mut conn, TENANT, USER, &expired.id)
        .await
        .expect("load");
    let recorded = reloaded
        .retention
        .deletion
        .expect("deletion reason recorded");
    assert_eq!(recorded.decision, DeletionDecision::Tombstoned);
    assert_eq!(recorded.reason, "retention_elapsed");

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn evidence_bundle_records_claims_sources_and_verification() {
    let Some((pool, name)) = scratch("artbundle").await else {
        blocked_marker();
        return;
    };
    seed_run(&pool).await;
    let objects = MemoryStore::default();
    let evidence_store = EvidenceStore::new(&objects);
    let bundle_store = EvidenceBundleStore;
    let mut conn = pool.acquire().await.expect("acquire");

    let evidence = evidence_store
        .record(
            &mut conn,
            TENANT,
            &NewEvidence {
                workspace_id: WORKSPACE.to_string(),
                run_id: Some(RUN.to_string()),
                step_id: None,
                effect_id: None,
                kind: EvidenceKind::HttpExchange,
                content: EvidenceContent::DigestOnly {
                    digest: Digest::of(b"fetched page"),
                },
                inline_summary: Some("200 OK".to_string()),
                captured_at: None,
                captured_by: CapturedBy {
                    kind: "browser".to_string(),
                    id: "browser-1".to_string(),
                },
                redaction_applied: true,
            },
        )
        .await
        .expect("record evidence");

    let bundle = bundle_store
        .record(
            &mut conn,
            TENANT,
            &NewEvidenceBundle {
                run_id: RUN.to_string(),
                artifact_version_id: None,
                claims: vec![Claim {
                    claim_id: "claim-1".to_string(),
                    text_span: "the page says hello".to_string(),
                    evidence_ids: vec![evidence.id.to_canonical()],
                    support_score: Some(1.0),
                }],
                sources: vec![Source {
                    source_id: "source-1".to_string(),
                    uri: Some("https://example.com/".to_string()),
                    artifact_ref: None,
                    retrieved_at: Some("2026-09-12T00:00:00Z".to_string()),
                    excerpt_locator: Some("p1".to_string()),
                    excerpt_digest: Some(Digest::of(b"fetched page").to_string()),
                }],
                coverage: Some(1.0),
                verification: Verification {
                    deterministic_passed: true,
                    semantic_score: Some(0.9),
                    verified_at: Some("2026-09-12T00:01:00Z".to_string()),
                },
            },
        )
        .await
        .expect("record bundle");

    let loaded = bundle_store
        .load(&mut conn, TENANT, &bundle.id)
        .await
        .expect("load bundle");
    assert_eq!(loaded, bundle);
    assert_eq!(loaded.claims.len(), 1);
    assert_eq!(
        loaded.claims[0].evidence_ids,
        vec![evidence.id.to_canonical()]
    );
    assert_eq!(loaded.sources.len(), 1);
    assert!(loaded.verification.deterministic_passed);
    assert_eq!(loaded.coverage, Some(1.0));

    drop_pool(&pool, &name).await;
}
