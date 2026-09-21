//! APP-011: knowledge, memory and skills through the API — a deleted or superseded entry is a
//! tombstone that leaves every retrieval path (the projection, the typed search, the derived
//! semantic index) with its lifecycle event staged; memory is projected from `memory_entries`
//! alone; a skill version climbs the §11.5 ladder one rung at a time and an unapproved
//! version cannot become ACTIVE; a run's projection records the active skills as an input
//! that grants nothing.
//!
//! Runs against a scratch database (blocked, not skipped, without one).

use axum::http::Request;
use quansio_server::api::ApiState;
use quansio_server::control::data_lifecycle::{SOURCE_KIND_KNOWLEDGE, SOURCE_KIND_MEMORY};
use quansio_server::control::schema;
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const KN_ACTIVE: &str = "kn_01J8Z3K6F1N8VQ2X5W9Y0KKKK1";
const KN_VERIFIED: &str = "kn_01J8Z3K6F1N8VQ2X5W9Y0KKKK2";
const KN_CANDIDATE: &str = "kn_01J8Z3K6F1N8VQ2X5W9Y0KKKK3";
const MEM_ACTIVE: &str = "mem_01J8Z3K6F1N8VQ2X5W9Y0KKKK1";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0K{n:04}")
}

struct Harness {
    router: axum::Router,
    pool: sqlx::PgPool,
    name: String,
}

async fn prepare(prefix: &str) -> Option<Harness> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0KKKKK', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }
    ]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    // Rows shaped as the intelligence plane writes them: two knowledge entries in force, one
    // candidate, one active memory, and a derived embedding for each entry in force.
    for (id, status, claim) in [
        (KN_ACTIVE, "active", "the evidence bundle must be signed"),
        (
            KN_VERIFIED,
            "verified",
            "the evidence bundle must be signed and consistent",
        ),
        (
            KN_CANDIDATE,
            "candidate",
            "the evidence bundle may be signed later",
        ),
    ] {
        sqlx::query(
            "INSERT INTO knowledge_entries (id, tenant_id, workspace_id, scope, kind, content, provenance, confidence, status) \
             VALUES ($1, $2, $3, 'workspace', 'fact', $4, $5, 0.9, $6)",
        )
        .bind(id)
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(json!({ "claim": claim }))
        .bind(json!([{ "source_kind": "approved_source", "ref": "doc:checklist", "digest": "0".repeat(64), "retrieved_at": "2026-09-20T00:00:00Z" }]))
        .bind(status)
        .execute(&mut *tx)
        .await
        .expect("knowledge");
    }
    sqlx::query(
        "INSERT INTO memory_entries (id, tenant_id, workspace_id, scope, subject_ref, content, provenance_kind, provenance_ref, confidence, status) \
         VALUES ($1, $2, $3, 'user', $4, 'prefers bundles summarised first', 'explicit_user', 'msg_seed', 0.8, 'active')",
    )
    .bind(MEM_ACTIVE)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(USER)
    .execute(&mut *tx)
    .await
    .expect("memory");
    let zero = format!("[{}]", vec!["0"; 1536].join(","));
    for (n, source_kind, source_ref) in [
        (1, SOURCE_KIND_KNOWLEDGE, KN_ACTIVE),
        (2, SOURCE_KIND_KNOWLEDGE, KN_VERIFIED),
        (3, SOURCE_KIND_MEMORY, MEM_ACTIVE),
    ] {
        sqlx::query(
            "INSERT INTO derived.embeddings (id, tenant_id, workspace_id, source_kind, source_ref, model_id, dimensions, embedding, snapshot, content_digest, chunk_index) \
             VALUES ($1, $2, $3, $4, $5, 'test-embedding', 1536, $6::vector, 'seed', $7, 0)",
        )
        .bind(format!("emb_01J8Z3K6F1N8VQ2X5W9Y0KKKK{n}"))
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(source_kind)
        .bind(source_ref)
        .bind(&zero)
        .bind("0".repeat(64))
        .execute(&mut *tx)
        .await
        .expect("embedding");
    }
    tx.commit().await.expect("commit");
    let state = ApiState::new(pool.clone()).expect("api");
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
    })
}

fn http(method: &str, path: &str, body: Option<Value>) -> Request<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER);
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(axum::body::Body::empty()).expect("request"),
    }
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
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
            Some(json!({ "command_id": command_id(id), "params": params })),
        ))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn get(router: &axum::Router, path: &str) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(http("GET", path, None))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn derived_rows(pool: &sqlx::PgPool, source_kind: &str, source_ref: &str) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM derived.embeddings WHERE tenant_id = $1 AND source_kind = $2 AND source_ref = $3",
    )
    .bind(TENANT)
    .bind(source_kind)
    .bind(source_ref)
    .fetch_one(&mut *tx)
    .await
    .expect("count");
    tx.commit().await.expect("commit");
    count
}

async fn events_for(pool: &sqlx::PgPool, aggregate_id: &str) -> Vec<String> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let types: Vec<String> = sqlx::query_scalar(
        "SELECT type FROM runtime_events WHERE tenant_id = $1 AND aggregate_id = $2 ORDER BY aggregate_version",
    )
    .bind(TENANT)
    .bind(aggregate_id)
    .fetch_all(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    types
}

fn ids(list: &Value) -> Vec<&str> {
    list.as_array()
        .expect("array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect()
}

#[tokio::test]
async fn a_deleted_or_superseded_entry_leaves_every_retrieval_path_and_stays_as_a_tombstone() {
    let Some(h) = prepare("knowledge_delete").await else {
        common::blocked_marker();
        return;
    };
    // Before: the projection shows provenance, scope and lifecycle; search finds both entries
    // in force; only ACTIVE is retrievable.
    let (status, listed) = get(
        &h.router,
        &format!("/v1/knowledge?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(ids(&listed["knowledge"]).len(), 3, "{listed}");
    let active = listed["knowledge"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["id"] == KN_ACTIVE)
        .expect("active row")
        .clone();
    assert_eq!(active["scope"], "workspace");
    assert_eq!(active["provenance"][0]["source_kind"], "approved_source");
    assert_eq!(active["retrievable"], true);
    assert_eq!(active["superseded_by"], Value::Null);
    let (_, found) = get(
        &h.router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=signed&channels=knowledge"),
    )
    .await;
    assert_eq!(found["hits"].as_array().map_or(0, Vec::len), 2, "{found}");
    assert_eq!(
        derived_rows(&h.pool, SOURCE_KIND_KNOWLEDGE, KN_ACTIVE).await,
        1
    );

    // Delete: the row is a tombstone, its derived rows are gone, the event is staged.
    let (status, deleted) = command(
        &h.router,
        "DeleteKnowledge",
        1,
        json!({ "knowledge_id": KN_ACTIVE }),
    )
    .await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["result"]["status"], "deleted", "{deleted}");
    assert_eq!(deleted["result"]["previous_status"], "active");
    assert_eq!(deleted["result"]["derived_rows_removed"], 1);
    assert_eq!(deleted["result"]["already"], false);
    let (_, after) = get(
        &h.router,
        &format!("/v1/knowledge?workspace_id={WORKSPACE}"),
    )
    .await;
    assert!(
        !ids(&after["knowledge"]).contains(&KN_ACTIVE),
        "deleted knowledge is not listed: {after}"
    );
    let (_, tombstones) = get(
        &h.router,
        &format!("/v1/knowledge?workspace_id={WORKSPACE}&include_deleted=true&status=deleted"),
    )
    .await;
    assert_eq!(
        ids(&tombstones["knowledge"]),
        vec![KN_ACTIVE],
        "{tombstones}"
    );
    assert_eq!(tombstones["knowledge"][0]["retrievable"], false);
    assert_eq!(
        tombstones["knowledge"][0]["provenance"][0]["ref"], "doc:checklist",
        "provenance survives the tombstone"
    );
    let (_, found) = get(
        &h.router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=signed&channels=knowledge"),
    )
    .await;
    assert_eq!(
        found["hits"].as_array().map_or(0, Vec::len),
        1,
        "search no longer answers with it: {found}"
    );
    assert_eq!(
        derived_rows(&h.pool, SOURCE_KIND_KNOWLEDGE, KN_ACTIVE).await,
        0,
        "the semantic channel lost it"
    );
    assert_eq!(
        events_for(&h.pool, KN_ACTIVE).await,
        vec!["knowledge.deleted"]
    );

    // Deleting again changes nothing and stages nothing; deletion is terminal.
    let (status, again) = command(
        &h.router,
        "DeleteKnowledge",
        2,
        json!({ "knowledge_id": KN_ACTIVE }),
    )
    .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["result"]["already"], true, "{again}");
    assert_eq!(events_for(&h.pool, KN_ACTIVE).await.len(), 1);
    let (status, refused) = command(
        &h.router,
        "SupersedeKnowledge",
        3,
        json!({ "knowledge_id": KN_ACTIVE, "superseded_by": KN_VERIFIED }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["code"], "CONFLICT_STATE", "{refused}");
    let (status, missing) = command(
        &h.router,
        "DeleteKnowledge",
        4,
        json!({ "knowledge_id": "kn_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ" }),
    )
    .await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["code"], "NOT_FOUND");
    let (status, malformed) = command(
        &h.router,
        "DeleteKnowledge",
        5,
        json!({ "knowledge_id": "mem_01J8Z3K6F1N8VQ2X5W9Y0KKKK1" }),
    )
    .await;
    assert_eq!(status, 400, "{malformed}");

    // Supersede: only in-force knowledge may be superseded, never by itself, and the old
    // entry points at its successor and leaves retrieval.
    let (status, candidate) = command(
        &h.router,
        "SupersedeKnowledge",
        6,
        json!({ "knowledge_id": KN_CANDIDATE, "superseded_by": KN_VERIFIED }),
    )
    .await;
    assert_eq!(status, 409, "a candidate is not in force: {candidate}");
    assert_eq!(candidate["code"], "CONFLICT_STATE");
    let (status, itself) = command(
        &h.router,
        "SupersedeKnowledge",
        7,
        json!({ "knowledge_id": KN_VERIFIED, "superseded_by": KN_VERIFIED }),
    )
    .await;
    assert_eq!(status, 400, "{itself}");
    assert_eq!(itself["code"], "VALIDATION_SCHEMA");
    let (status, superseded) = command(
        &h.router,
        "SupersedeKnowledge",
        8,
        json!({ "knowledge_id": KN_VERIFIED, "superseded_by": KN_CANDIDATE }),
    )
    .await;
    assert_eq!(status, 200, "{superseded}");
    assert_eq!(superseded["result"]["status"], "superseded");
    assert_eq!(superseded["result"]["superseded_by"], KN_CANDIDATE);
    assert_eq!(superseded["result"]["derived_rows_removed"], 1);
    let (_, listed) = get(
        &h.router,
        &format!("/v1/knowledge?workspace_id={WORKSPACE}&status=superseded"),
    )
    .await;
    assert_eq!(ids(&listed["knowledge"]), vec![KN_VERIFIED], "{listed}");
    assert_eq!(listed["knowledge"][0]["superseded_by"], KN_CANDIDATE);
    assert_eq!(listed["knowledge"][0]["retrievable"], false);
    let (_, found) = get(
        &h.router,
        &format!("/v1/search?workspace_id={WORKSPACE}&q=signed&channels=knowledge"),
    )
    .await;
    assert_eq!(found["hits"].as_array().map_or(0, Vec::len), 0, "{found}");
    assert_eq!(
        derived_rows(&h.pool, SOURCE_KIND_KNOWLEDGE, KN_VERIFIED).await,
        0
    );
    assert_eq!(
        events_for(&h.pool, KN_VERIFIED).await,
        vec!["knowledge.superseded"]
    );

    // Memory: projected from memory_entries alone, with provenance and scope, and deleted the
    // same way — the tombstone stays, the derived rows go, the event is staged.
    let (status, memory) = get(&h.router, &format!("/v1/memory?workspace_id={WORKSPACE}")).await;
    assert_eq!(status, 200, "{memory}");
    assert_eq!(ids(&memory["memory"]), vec![MEM_ACTIVE], "{memory}");
    let row = &memory["memory"][0];
    let mut keys: Vec<&str> = row
        .as_object()
        .expect("row")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "confidence",
            "content",
            "created_at",
            "expires_at",
            "id",
            "last_used_at",
            "provenance",
            "retrievable",
            "scope",
            "status",
            "subject_ref",
            "updated_at",
            "workspace_id"
        ],
        "memory carries no recovery field"
    );
    assert_eq!(
        row["provenance"],
        json!({ "kind": "explicit_user", "ref": "msg_seed" })
    );
    assert_eq!(row["scope"], "user");
    assert_eq!(row["retrievable"], true);
    let (_, by_subject) = get(
        &h.router,
        &format!("/v1/memory?workspace_id={WORKSPACE}&scope=user&subject_ref={USER}"),
    )
    .await;
    assert_eq!(ids(&by_subject["memory"]), vec![MEM_ACTIVE]);
    let (_, none) = get(
        &h.router,
        &format!("/v1/memory?workspace_id={WORKSPACE}&scope=teammate"),
    )
    .await;
    assert_eq!(ids(&none["memory"]), Vec::<&str>::new());
    let (status, forgotten) = command(
        &h.router,
        "DeleteMemory",
        9,
        json!({ "memory_id": MEM_ACTIVE }),
    )
    .await;
    assert_eq!(status, 200, "{forgotten}");
    assert_eq!(forgotten["result"]["status"], "deleted");
    assert_eq!(forgotten["result"]["derived_rows_removed"], 1);
    let (_, memory) = get(&h.router, &format!("/v1/memory?workspace_id={WORKSPACE}")).await;
    assert_eq!(
        ids(&memory["memory"]),
        Vec::<&str>::new(),
        "deleted memory is not listed: {memory}"
    );
    let (_, tombstones) = get(
        &h.router,
        &format!("/v1/memory?workspace_id={WORKSPACE}&include_deleted=true"),
    )
    .await;
    assert_eq!(tombstones["memory"][0]["status"], "deleted");
    assert_eq!(tombstones["memory"][0]["retrievable"], false);
    assert_eq!(
        derived_rows(&h.pool, SOURCE_KIND_MEMORY, MEM_ACTIVE).await,
        0
    );
    assert_eq!(
        events_for(&h.pool, MEM_ACTIVE).await,
        vec!["memory.deleted"]
    );
    let (status, again) = command(
        &h.router,
        "DeleteMemory",
        10,
        json!({ "memory_id": MEM_ACTIVE }),
    )
    .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["result"]["already"], true);

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_skill_version_climbs_the_ladder_one_rung_at_a_time_and_grants_nothing() {
    let Some(h) = prepare("knowledge_skills").await else {
        common::blocked_marker();
        return;
    };
    // Install: a skill with one DRAFT version; installing never activates.
    let (status, installed) = command(
        &h.router,
        "InstallSkill",
        1,
        json!({
            "scope": "workspace",
            "name": "release-notes",
            "version": { "semver": "1.0.0", "manifest": { "instructions": "summarise the bundle", "tool_needs": ["fs.read"], "capability_needs": [{ "effect_class": "fs.write.workspace", "resource": { "kind": "fs", "selector": "**" } }] }, "provenance": "authored:e2e" }
        }),
    )
    .await;
    assert_eq!(status, 200, "{installed}");
    let skill_id = installed["result"]["skill_id"]
        .as_str()
        .expect("skill")
        .to_string();
    let version_id = installed["result"]["version"]["skill_version_id"]
        .as_str()
        .expect("version")
        .to_string();
    assert_eq!(
        installed["result"]["owner"], USER,
        "the installing user owns it unless told otherwise"
    );
    assert_eq!(installed["result"]["version"]["status"], "draft");
    assert_eq!(installed["result"]["version"]["resolves"], false);
    assert_eq!(
        installed["result"]["version"]["promotable_to"],
        json!(["candidate"])
    );
    let (status, listed) = get(&h.router, "/v1/skills").await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["skills"][0]["id"], skill_id);
    assert_eq!(
        listed["skills"][0]["current_active_version_id"],
        Value::Null
    );
    assert_eq!(listed["skills"][0]["versions"][0]["id"], version_id);
    assert_eq!(
        listed["skills"][0]["versions"][0]["manifest"]["capability_needs"][0]["effect_class"],
        "fs.write.workspace"
    );

    // An unapproved version cannot become ACTIVE: the ladder refuses, nothing changes.
    let (status, refused) = command(
        &h.router,
        "PromoteSkillVersion",
        2,
        json!({ "skill_version_id": version_id, "to": "active" }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["code"], "RUNTIME_ILLEGAL_TRANSITION", "{refused}");
    let (_, listed) = get(&h.router, "/v1/skills").await;
    assert_eq!(listed["skills"][0]["versions"][0]["status"], "draft");
    assert_eq!(
        listed["skills"][0]["current_active_version_id"],
        Value::Null
    );
    let (status, unknown) = command(
        &h.router,
        "PromoteSkillVersion",
        3,
        json!({ "skill_version_id": version_id, "to": "shipped" }),
    )
    .await;
    assert_eq!(status, 400, "{unknown}");

    // Every rung, in order; only the last one resolves.
    let mut n = 4;
    for (to, resolves) in [
        ("candidate", false),
        ("evaluating", false),
        ("approved", false),
        ("active", true),
    ] {
        let (status, promoted) = command(
            &h.router,
            "PromoteSkillVersion",
            n,
            json!({ "skill_version_id": version_id, "to": to }),
        )
        .await;
        assert_eq!(status, 200, "{promoted}");
        assert_eq!(promoted["result"]["status"], to, "{promoted}");
        assert_eq!(promoted["result"]["resolves"], resolves, "{promoted}");
        n += 1;
    }
    let (_, listed) = get(&h.router, "/v1/skills").await;
    assert_eq!(
        listed["skills"][0]["current_active_version_id"], version_id,
        "{listed}"
    );
    assert_eq!(listed["skills"][0]["versions"][0]["resolves"], true);
    assert_eq!(
        listed["skills"][0]["versions"][0]["promotable_to"],
        json!(["deprecated"])
    );

    // A run's authority records the active skill as an input and takes no grant from it:
    // the manifest asks for fs.write.workspace and the projection still allows read only.
    let (status, posted) = command(
        &h.router,
        "PostMessage",
        n,
        json!({ "workspace_id": WORKSPACE, "content": "hello" }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    n += 1;
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let (status, run) = get(&h.router, &format!("/v1/runs/{run_id}")).await;
    assert_eq!(status, 200, "{run}");
    let projection = &run["capability_projection"];
    assert_eq!(
        projection["subject"],
        json!({ "kind": "run", "id": run_id })
    );
    let inputs = projection["inputs"].as_array().expect("inputs");
    let layers: Vec<&str> = inputs
        .iter()
        .filter_map(|input| input["layer"].as_str())
        .collect();
    // The workspace's default teammate (APP-014) is an input between the policy and the
    // skills, in the fixed layer order; it declares no needs, so it narrows nothing.
    assert_eq!(
        layers,
        vec!["tenant_policy", "agent_definition", "active_skills"],
        "{run}"
    );
    assert_eq!(inputs[2]["ref"], version_id, "{run}");
    assert!(
        inputs[1]["ref"]
            .as_str()
            .is_some_and(|reference| reference.starts_with("agt_")),
        "{run}"
    );
    assert_eq!(
        inputs[0]["ref"], "pol_01J8Z3K6F1N8VQ2X5W9Y0KKKKK@1",
        "{run}"
    );
    let classes: Vec<&str> = projection["grants"]
        .as_array()
        .expect("grants")
        .iter()
        .filter_map(|grant| grant["effect_class"].as_str())
        .collect();
    assert_eq!(
        classes,
        vec!["read.internal"],
        "a skill grants nothing: {run}"
    );
    assert_eq!(projection["inputs_digest"].as_str().map(str::len), Some(64));

    // Disable: ACTIVE → DEPRECATED clears the pointer; a skill with no active version is refused.
    let (status, disabled) = command(
        &h.router,
        "DisableSkill",
        n,
        json!({ "skill_id": skill_id }),
    )
    .await;
    assert_eq!(status, 200, "{disabled}");
    n += 1;
    assert_eq!(disabled["result"]["status"], "deprecated");
    assert_eq!(disabled["result"]["previous_active_version_id"], version_id);
    let (_, listed) = get(&h.router, "/v1/skills").await;
    assert_eq!(
        listed["skills"][0]["current_active_version_id"],
        Value::Null,
        "{listed}"
    );
    assert_eq!(listed["skills"][0]["versions"][0]["resolves"], false);
    let (status, nothing) = command(
        &h.router,
        "DisableSkill",
        n,
        json!({ "skill_id": skill_id }),
    )
    .await;
    assert_eq!(status, 409, "{nothing}");
    n += 1;
    assert_eq!(nothing["code"], "CONFLICT_STATE");
    // A deprecated version cannot be revived.
    let (status, revived) = command(
        &h.router,
        "PromoteSkillVersion",
        n,
        json!({ "skill_version_id": version_id, "to": "active" }),
    )
    .await;
    assert_eq!(status, 409, "{revived}");
    n += 1;
    // The next run's projection no longer names the skill.
    let (_, posted) = command(
        &h.router,
        "PostMessage",
        n,
        json!({ "workspace_id": WORKSPACE, "content": "again" }),
    )
    .await;
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let (_, run) = get(&h.router, &format!("/v1/runs/{run_id}")).await;
    let layers: Vec<&str> = run["capability_projection"]["inputs"]
        .as_array()
        .expect("inputs")
        .iter()
        .filter_map(|input| input["layer"].as_str())
        .collect();
    assert_eq!(layers, vec!["tenant_policy", "agent_definition"], "{run}");

    common::drop_pool(&h.pool, &h.name).await;
}
