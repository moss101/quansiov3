//! APP-006: approvals, effects and evidence — an approval is bound to the previewed digest
//! (a changed or stale one is refused and nothing is granted), a granted approval resumes
//! the run and the call dispatches under its single-use receipt, a denied one is refused
//! and recorded, unsettled effects stay visibly unresolved in the projection, and evidence
//! bytes that no longer hash to their digest are a typed error, never bytes.
//!
//! Runs against a scratch database and the dev MinIO (blocked, not skipped, without them).

use std::sync::Arc;

use axum::http::Request;
use quansio_core::Digest;
use quansio_server::api::seams::APPROVAL_SCENARIO;
use quansio_server::api::ApiState;
use quansio_server::artifacts::{
    CapturedBy, EvidenceContent, EvidenceKind, EvidenceStore, NewEvidence, ObjectKey, ObjectStore,
    S3ObjectStore,
};
use quansio_server::control::schema;
use quansio_server::effects::EffectLedger;
use quansio_server::runtime::state_machine::RuntimeIdentity;
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0PPPPP";
const SIGNING_KEY: &str = "trust-surface-test-signing-key";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0P{n:04}")
}

struct Harness {
    router: axum::Router,
    pool: sqlx::PgPool,
    name: String,
    objects: Arc<dyn ObjectStore>,
}

async fn prepare(prefix: &str) -> Option<Harness> {
    // The approval signing key: RUN-006 signs receipts with it and the resumed turn verifies.
    std::env::set_var("QUANSIO_APPROVAL_SIGNING_KEY", SIGNING_KEY);
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    // Reads are allowed; a write asks a human first. Without an execution target the
    // runtime classifies `fs.write` as the stricter host write (tier 3), so both classes
    // are governed.
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "fs.write.workspace", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" },
        { "effect_class": "fs.write.host", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" }
    ]);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0PPPPP', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    let objects: Arc<dyn ObjectStore> =
        Arc::new(S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration"));
    let state = ApiState::new(pool.clone())
        .expect("api")
        .with_object_store(objects.clone());
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
        objects,
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

#[tokio::test]
async fn an_approval_is_bound_to_the_previewed_digest_and_a_grant_dispatches_the_call() {
    let Some(h) = prepare("trust_approve").await else {
        common::blocked_marker();
        return;
    };
    // The write needs a human: the run parks and a request with a typed preview exists.
    let (status, posted) = command(
        &h.router,
        "PostMessage",
        1,
        json!({ "workspace_id": WORKSPACE, "content": format!("write the summary {APPROVAL_SCENARIO}") }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let thread_id = posted["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    eprintln!(
        "DEBUG effects {}",
        get(&h.router, &format!("/v1/effects?run_id={run_id}"))
            .await
            .1
    );
    eprintln!(
        "DEBUG timeline {}",
        get(&h.router, &format!("/v1/runs/{run_id}/timeline"))
            .await
            .1
    );
    assert_eq!(posted["result"]["turn"]["kind"], "parked", "{posted}");

    let (status, listed) = get(&h.router, &format!("/v1/approvals?run_id={run_id}")).await;
    assert_eq!(status, 200, "{listed}");
    let request = listed["approvals"][0].clone();
    assert_eq!(request["status"], "requested", "{listed}");
    assert_eq!(request["run_id"], run_id, "{listed}");
    let request_id = request["id"].as_str().expect("request id").to_string();
    let digest = request["params_digest"]
        .as_str()
        .expect("digest")
        .to_string();
    assert_eq!(digest.len(), 64, "a bare sha256 hex digest: {listed}");
    // The tool's source trust is untrusted-external, so the preview names the origin the
    // operator is approving on behalf of (INT-012).
    assert_eq!(
        request["consequence_preview"]["untrusted_origin"]["trust"], "UNTRUSTED_EXTERNAL",
        "{listed}"
    );
    assert!(
        request["effect"]["class"]
            .as_str()
            .is_some_and(|c| c.starts_with("fs.write.")),
        "{listed}"
    );
    assert_eq!(request["effect"]["status"], "PROPOSED", "{listed}");
    assert_eq!(request["effect"]["tool_name"], "fs.write", "{listed}");
    assert!(
        request["consequence_preview"]["targets"].is_array(),
        "{listed}"
    );
    assert!(request["expires_at"].is_string(), "{listed}");
    assert_eq!(request["receipts"], json!([]), "{listed}");
    let (_, by_workspace) = get(
        &h.router,
        &format!("/v1/approvals?workspace_id={WORKSPACE}&status=requested"),
    )
    .await;
    assert_eq!(
        by_workspace["approvals"][0]["id"], request_id,
        "{by_workspace}"
    );

    // The effect is PROPOSED in the ledger's projection, with its digest and target.
    let (_, effects) = get(&h.router, &format!("/v1/effects?run_id={run_id}")).await;
    let effect = effects["effects"][0].clone();
    assert_eq!(effect["status"], "PROPOSED", "{effects}");
    assert_eq!(effect["params_digest"], digest, "{effects}");
    assert!(
        effect["effect_class"]
            .as_str()
            .is_some_and(|c| c.starts_with("fs.write.")),
        "{effects}"
    );
    assert!(effect["target"].is_object(), "{effects}");
    assert_eq!(effect["unsettled"], false, "{effects}");

    // A digest that is not the previewed one is refused: the payload the user saw is what
    // they approve, nothing else. Nothing is granted.
    let other = Digest::of(b"something else").to_string();
    let (status, refused) = command(
        &h.router,
        "ApproveEffect",
        2,
        json!({ "approval_request_id": request_id, "params_digest": other }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["code"], "APPROVAL_SUPERSEDED", "{refused}");
    let (_, still) = get(&h.router, &format!("/v1/approvals?run_id={run_id}")).await;
    assert_eq!(still["approvals"][0]["status"], "requested", "{still}");
    assert_eq!(still["approvals"][0]["receipts"], json!([]), "{still}");

    // The previewed digest: a signed receipt, the run resumes, the call dispatches and
    // settles under that receipt; the reply follows in the thread.
    let (status, granted) = command(
        &h.router,
        "ApproveEffect",
        3,
        json!({ "approval_request_id": request_id, "params_digest": digest }),
    )
    .await;
    assert_eq!(status, 200, "{granted}");
    assert_eq!(granted["result"]["params_digest"], digest, "{granted}");
    assert!(
        granted["result"]["receipt_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("apr_") || id.starts_with("rcp_")),
        "{granted}"
    );
    assert_eq!(granted["result"]["turn"]["kind"], "completed", "{granted}");
    let (_, after) = get(&h.router, &format!("/v1/approvals?run_id={run_id}")).await;
    assert_eq!(after["approvals"][0]["status"], "granted", "{after}");
    assert_eq!(
        after["approvals"][0]["receipts"].as_array().map(Vec::len),
        Some(1),
        "{after}"
    );
    assert_eq!(
        after["approvals"][0]["receipts"][0]["consumed"], true,
        "the receipt was consumed by the dispatch: {after}"
    );
    let (_, settled) = get(&h.router, &format!("/v1/effects?run_id={run_id}")).await;
    let effect = settled["effects"][0].clone();
    assert_eq!(effect["status"], "SETTLED_SUCCESS", "{settled}");
    assert!(effect["approval_receipt_id"].is_string(), "{settled}");
    assert!(effect["outcome"].is_object(), "{settled}");
    assert_eq!(effect["terminal"], true, "{settled}");
    let (_, messages) = get(&h.router, &format!("/v1/messages?thread_id={thread_id}")).await;
    let reply = messages["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["author_kind"] == "agent")
        .expect("reply");
    assert_eq!(
        reply["content_blocks"][0]["text"], "the file is written.",
        "{messages}"
    );

    // A second approval of the same request is a state conflict, not a second receipt.
    let (status, again) = command(
        &h.router,
        "ApproveEffect",
        4,
        json!({ "approval_request_id": request_id, "params_digest": digest }),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["code"], "CONFLICT_STATE", "{again}");

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_denied_approval_records_the_effect_as_denied_and_the_run_moves_on() {
    let Some(h) = prepare("trust_deny").await else {
        common::blocked_marker();
        return;
    };
    let (status, posted) = command(
        &h.router,
        "PostMessage",
        1,
        json!({ "workspace_id": WORKSPACE, "content": format!("write it {APPROVAL_SCENARIO}") }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let (_, listed) = get(&h.router, &format!("/v1/approvals?run_id={run_id}")).await;
    let request_id = listed["approvals"][0]["id"]
        .as_str()
        .expect("request")
        .to_string();
    let digest = listed["approvals"][0]["params_digest"]
        .as_str()
        .expect("digest")
        .to_string();

    let (status, denied) = command(
        &h.router,
        "DenyEffect",
        2,
        json!({ "approval_request_id": request_id }),
    )
    .await;
    assert_eq!(status, 200, "{denied}");
    assert_eq!(denied["result"]["effect_status"], "DENIED", "{denied}");
    let (_, after) = get(&h.router, &format!("/v1/approvals?run_id={run_id}")).await;
    assert_eq!(after["approvals"][0]["status"], "denied", "{after}");
    let (_, effects) = get(&h.router, &format!("/v1/effects?run_id={run_id}")).await;
    assert_eq!(effects["effects"][0]["status"], "DENIED", "{effects}");
    // Approving a denied request is refused; nothing was ever dispatched.
    let (status, late) = command(
        &h.router,
        "ApproveEffect",
        3,
        json!({ "approval_request_id": request_id, "params_digest": digest }),
    )
    .await;
    assert_eq!(status, 409, "{late}");
    let (_, run) = get(&h.router, &format!("/v1/runs/{run_id}")).await;
    assert_ne!(run["status"], "WAITING_APPROVAL", "{run}");

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn unsettled_effects_stay_visibly_unresolved() {
    let Some(h) = prepare("trust_unknown").await else {
        common::blocked_marker();
        return;
    };
    let (status, posted) = command(
        &h.router,
        "PostMessage",
        1,
        json!({ "workspace_id": WORKSPACE, "content": "read the notes" }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let (_, effects) = get(&h.router, &format!("/v1/effects?run_id={run_id}")).await;
    let effect_id = effects["effects"][0]["id"]
        .as_str()
        .expect("effect")
        .to_string();
    assert_eq!(
        effects["effects"][0]["status"], "SETTLED_SUCCESS",
        "{effects}"
    );

    // A second effect the ledger owner lost track of: dispatched, then outcome unknown.
    let identity = RuntimeIdentity::system(
        TENANT,
        "trust-test",
        quansio_core::CorrelationId::generate(&mut quansio_core::UlidGenerator::new()),
    );
    let ledger = EffectLedger::new(h.pool.clone(), identity).expect("ledger");
    let record = ledger.load(&effect_id).await.expect("load");
    let unknown = ledger
        .reserve(quansio_server::effects::NewEffect::policy_authorized(
            WORKSPACE,
            record.effect_class.clone(),
            record.tier,
            record.resource.clone(),
            Digest::of(b"another call"),
            record.capability_projection_id.clone(),
            record
                .policy_decision_id
                .clone()
                .unwrap_or_else(|| "pdc_test".to_string()),
            record.target.clone().expect("target"),
            record.generation,
        ))
        .await
        .expect("reserve");
    let unknown = ledger
        .mark_dispatched(
            &unknown.id,
            unknown.dispatch_token.as_deref().unwrap_or("tok_test"),
        )
        .await
        .expect("dispatch");
    let unknown = ledger
        .mark_outcome_unknown(&unknown.id, Some("worker connection lost".to_string()))
        .await
        .expect("unknown");
    assert_eq!(unknown.status.as_str(), "OUTCOME_UNKNOWN");

    let (_, listed) = get(&h.router, &format!("/v1/effects?workspace_id={WORKSPACE}")).await;
    let row = listed["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == unknown.id)
        .cloned()
        .expect("listed");
    assert_eq!(row["status"], "OUTCOME_UNKNOWN", "{listed}");
    assert_eq!(row["unsettled"], true, "{listed}");
    assert_eq!(row["terminal"], false, "{listed}");
    let (_, only_unknown) = get(
        &h.router,
        &format!("/v1/effects?workspace_id={WORKSPACE}&status=OUTCOME_UNKNOWN"),
    )
    .await;
    assert_eq!(
        only_unknown["effects"].as_array().map(Vec::len),
        Some(1),
        "{only_unknown}"
    );
    let (status, bad) = get(
        &h.router,
        &format!("/v1/effects?workspace_id={WORKSPACE}&status=WHATEVER"),
    )
    .await;
    assert_eq!(status, 400, "{bad}");

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn evidence_bytes_that_do_not_hash_to_their_digest_are_a_typed_error_not_bytes() {
    let Some(h) = prepare("trust_evidence").await else {
        common::blocked_marker();
        return;
    };
    let mut conn = h.pool.acquire().await.expect("conn");
    let store = EvidenceStore::new(h.objects.as_ref());
    let evidence = store
        .record(
            &mut conn,
            TENANT,
            &NewEvidence {
                workspace_id: WORKSPACE.to_string(),
                run_id: None,
                step_id: None,
                effect_id: None,
                kind: EvidenceKind::ToolOutput,
                content: EvidenceContent::Bytes {
                    bytes: b"exit 0\nall tests passed\n".to_vec(),
                    media_type: "text/plain".to_string(),
                },
                inline_summary: Some("tests passed".to_string()),
                captured_at: None,
                captured_by: CapturedBy {
                    kind: "run".to_string(),
                    id: "run_test".to_string(),
                },
                redaction_applied: false,
            },
        )
        .await
        .expect("evidence");
    let evidence_id = evidence.id.to_canonical();

    // Intact: the bytes come back with the digest they were recorded under.
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/evidence/{evidence_id}/content"),
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-quansio-digest")
            .and_then(|v| v.to_str().ok()),
        Some(evidence.content_digest.to_string().as_str())
    );
    assert_eq!(
        response
            .headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    assert_eq!(bytes.as_ref(), b"exit 0\nall tests passed\n");
    assert_eq!(Digest::of(&bytes), evidence.content_digest);

    // Tampered in the store: the digest no longer matches, so the API refuses with typed
    // details rather than handing out bytes it cannot vouch for.
    let key: ObjectKey = evidence.object_key.clone().expect("object key");
    h.objects
        .put(&key, b"exit 1\nall tests passed\n", "text/plain")
        .await
        .expect("tamper");
    let response = h
        .router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/evidence/{evidence_id}/content"),
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 500);
    let body = json_body(response).await;
    assert_eq!(body["code"], "INTERNAL", "{body}");
    assert_eq!(body["details"]["kind"], "digest_mismatch", "{body}");
    assert_eq!(
        body["details"]["expected"],
        evidence.content_digest.to_string(),
        "{body}"
    );
    assert_ne!(
        body["details"]["actual"],
        evidence.content_digest.to_string(),
        "{body}"
    );

    common::drop_pool(&h.pool, &h.name).await;
}
