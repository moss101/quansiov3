//! Database-backed rebuild/maintenance tests for `quansio-indexer`.
//!
//! The authoritative source is real PostgreSQL: tests create a scratch database,
//! apply the canonical migrations, and seed `artifacts` / `artifact_versions` rows.
//! Index files live in a local temporary directory. Text bytes are supplied by an
//! in-process provider that stands in for the artifact object store; the index never
//! owns those bytes.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (superuser DSN used to create scratch
//! databases). Absent → every test prints `BLOCKED_EXTERNAL` and returns.

mod common;

use std::collections::HashMap;

use async_trait::async_trait;
use quansio_indexer::{
    Channel, IndexResult, ObjectTextProvider, PostgresArtifactSource, SearchIndex, SearchProgram,
    SearchScope, SourceChange, SourceDocument, SourceScope,
};
use sqlx::PgPool;
use tempfile::TempDir;

const TENANT_A: &str = "tn_alpha";
const TENANT_B: &str = "tn_beta";
const WORKSPACE: &str = "ws_alpha";

/// In-process stand-in for reading artifact bytes; the index owns no object store.
#[derive(Default)]
struct MemoryText {
    entries: HashMap<String, String>,
}

impl MemoryText {
    fn add(mut self, object_key: &str, text: &str) -> Self {
        self.entries
            .insert(object_key.to_string(), text.to_string());
        self
    }
}

#[async_trait]
impl ObjectTextProvider for MemoryText {
    async fn text_for(&self, object_key: &str, _media_type: &str) -> IndexResult<Option<String>> {
        Ok(self.entries.get(object_key).cloned())
    }
}

fn program(tenant: &str, query: &str) -> SearchProgram {
    SearchProgram::new(SearchScope::tenant(tenant), query).with_channels(vec![Channel::Lexical])
}

async fn mark_artifact_deleted(pool: &PgPool, artifact: &str) {
    sqlx::query("UPDATE artifacts SET deleted_at = now() WHERE id = $1")
        .bind(artifact)
        .execute(pool)
        .await
        .expect("mark artifact deleted");
}

#[allow(clippy::too_many_arguments)]
async fn add_artifact_version(
    pool: &PgPool,
    tenant: &str,
    artifact: &str,
    version: &str,
    seq: i32,
    object_key: &str,
    digest: &str,
) {
    sqlx::query(
        "INSERT INTO artifact_versions \
         (id, tenant_id, artifact_id, seq, content_digest, size_bytes, media_type, object_key) \
         VALUES ($1, $2, $3, $4, $5, 0, 'text/markdown', $6)",
    )
    .bind(version)
    .bind(tenant)
    .bind(artifact)
    .bind(seq)
    .bind(digest)
    .bind(object_key)
    .execute(pool)
    .await
    .expect("insert artifact version");
}

#[tokio::test]
async fn rebuild_from_authoritative_rows_is_deterministic_and_tenant_isolated() {
    let name = common::scratch_name("rebuild");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    common::seed_artifact(
        &pool,
        TENANT_A,
        WORKSPACE,
        "art_alpha",
        "artv_alpha1",
        "Alpha orbital notes",
        "tenants/tn_alpha/art_alpha/v1",
        "digest_alpha1",
        "text/markdown",
    )
    .await;
    common::seed_artifact(
        &pool,
        TENANT_B,
        WORKSPACE,
        "art_beta",
        "artv_beta1",
        "Beta orbital notes",
        "tenants/tn_beta/art_beta/v1",
        "digest_beta1",
        "text/markdown",
    )
    .await;
    let text = MemoryText::default()
        .add(
            "tenants/tn_alpha/art_alpha/v1",
            "orbital mechanics for alpha",
        )
        .add("tenants/tn_beta/art_beta/v1", "orbital mechanics for beta");
    let directory = TempDir::new().expect("temp dir");
    let index = SearchIndex::open(directory.path()).expect("open index");
    let source = PostgresArtifactSource::new(pool.clone(), text);

    let first = index
        .rebuild_from(&source, &SourceScope::tenant(TENANT_A))
        .await
        .expect("rebuild alpha");
    assert_eq!(first.document_count, 1);
    let alpha_hits = index
        .search(&program(TENANT_A, "orbital"))
        .expect("alpha search");
    assert_eq!(alpha_hits.results.len(), 1);
    assert_eq!(alpha_hits.results[0].source_id, "art_alpha");
    assert_eq!(
        alpha_hits.results[0].locator,
        "artifact_version:artv_alpha1"
    );
    assert_eq!(alpha_hits.results[0].snapshot, first.snapshot.as_string());
    assert_eq!(alpha_hits.results[0].content_digest, "digest_alpha1");

    index
        .rebuild_from(&source, &SourceScope::tenant(TENANT_B))
        .await
        .expect("rebuild beta");
    let beta_hits = index
        .search(&program(TENANT_B, "orbital"))
        .expect("beta search");
    assert_eq!(beta_hits.results.len(), 1);
    assert_eq!(beta_hits.results[0].source_id, "art_beta");
    // Tenant A never sees tenant B's document.
    assert!(index
        .search(&program(TENANT_A, "for beta"))
        .expect("cross search")
        .results
        .is_empty());

    // Rebuilding the identical corpus reproduces the same snapshot and same hits.
    let again = index
        .rebuild_from(&source, &SourceScope::tenant(TENANT_A))
        .await
        .expect("rebuild alpha again");
    assert_eq!(first.snapshot, again.snapshot);
    let alpha_replay = index
        .search(&program(TENANT_A, "orbital"))
        .expect("alpha replay");
    assert_eq!(alpha_hits.results, alpha_replay.results);

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn maintenance_reflects_updates_deletes_and_idempotent_replay() {
    let name = common::scratch_name("maint");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    common::seed_artifact(
        &pool,
        TENANT_A,
        WORKSPACE,
        "art_alpha",
        "artv_alpha1",
        "Notes one",
        "tenants/tn_alpha/art_alpha/v1",
        "digest_alpha1",
        "text/markdown",
    )
    .await;
    let text = MemoryText::default().add("tenants/tn_alpha/art_alpha/v1", "alpha horizon");
    let directory = TempDir::new().expect("temp dir");
    let index = SearchIndex::open(directory.path()).expect("open index");
    let source = PostgresArtifactSource::new(pool.clone(), text);
    index
        .rebuild_from(&source, &SourceScope::tenant(TENANT_A))
        .await
        .expect("rebuild");
    assert_eq!(
        index
            .search(&program(TENANT_A, "horizon"))
            .expect("search")
            .results
            .len(),
        1
    );

    // Incremental update: the same version now carries new text.
    let updated = SourceDocument::artifact(
        TENANT_A,
        WORKSPACE,
        "art_alpha",
        "artv_alpha1",
        "Notes one",
        "delta horizon",
        "text/markdown",
        "digest_alpha2",
    );
    let applied = index
        .apply(SourceChange::upsert(updated.clone()))
        .expect("upsert");
    assert!(applied.applied);
    assert!(
        index
            .search(&program(TENANT_A, "delta"))
            .expect("new text")
            .results
            .len()
            == 1
    );
    assert!(index
        .search(&program(TENANT_A, "alpha"))
        .expect("old text")
        .results
        .is_empty());

    // Re-applying the identical change is a no-op.
    let replay = index
        .apply(SourceChange::upsert(updated))
        .expect("idempotent upsert");
    assert!(!replay.applied);
    assert_eq!(replay.snapshot, applied.snapshot);

    // Incremental delete removes the terms; replaying the delete is a no-op.
    let deleted = index
        .apply(SourceChange::delete(TENANT_A, "artv_alpha1"))
        .expect("delete");
    assert!(deleted.applied);
    assert!(index
        .search(&program(TENANT_A, "delta"))
        .expect("after delete")
        .results
        .is_empty());
    assert!(
        !index
            .apply(SourceChange::delete(TENANT_A, "artv_alpha1"))
            .expect("idempotent delete")
            .applied
    );

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn rebuild_propagates_authoritative_new_versions_and_deletions() {
    let name = common::scratch_name("propagate");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    common::seed_artifact(
        &pool,
        TENANT_A,
        WORKSPACE,
        "art_gamma",
        "artv_gamma1",
        "Gamma notes",
        "tenants/tn_alpha/art_gamma/v1",
        "digest_gamma1",
        "text/markdown",
    )
    .await;
    let text = MemoryText::default()
        .add("tenants/tn_alpha/art_gamma/v1", "gamma horizon")
        .add("tenants/tn_alpha/art_gamma/v2", "epsilon horizon");
    let directory = TempDir::new().expect("temp dir");
    let index = SearchIndex::open(directory.path()).expect("open index");
    let source = PostgresArtifactSource::new(pool.clone(), text);

    index
        .rebuild_from(&source, &SourceScope::tenant(TENANT_A))
        .await
        .expect("first rebuild");
    assert_eq!(
        index
            .search(&program(TENANT_A, "gamma"))
            .expect("gamma")
            .results
            .len(),
        1
    );

    // A new immutable version appears: both versions are indexed projections.
    add_artifact_version(
        &pool,
        TENANT_A,
        "art_gamma",
        "artv_gamma2",
        2,
        "tenants/tn_alpha/art_gamma/v2",
        "digest_gamma2",
    )
    .await;
    index
        .rebuild_from(&source, &SourceScope::tenant(TENANT_A))
        .await
        .expect("second rebuild");
    let epsilon = index
        .search(&program(TENANT_A, "epsilon"))
        .expect("epsilon");
    assert_eq!(epsilon.results.len(), 1);
    assert_eq!(epsilon.results[0].locator, "artifact_version:artv_gamma2");

    // Deleting the artifact removes every one of its versions from retrieval.
    mark_artifact_deleted(&pool, "art_gamma").await;
    index
        .rebuild_from(&source, &SourceScope::tenant(TENANT_A))
        .await
        .expect("third rebuild");
    assert!(index
        .search(&program(TENANT_A, "epsilon"))
        .expect("epsilon after delete")
        .results
        .is_empty());
    assert!(index
        .search(&program(TENANT_A, "gamma"))
        .expect("gamma after delete")
        .results
        .is_empty());

    common::drop_pool(&pool, &name).await;
}
