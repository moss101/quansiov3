//! APP-004: the conversation surface over the runtime — questions asked and answered in a
//! thread, a cancelled turn, an upload that becomes an attachment, and the live frames a
//! subscriber sees while a turn runs.
//!
//! Every test runs against a scratch database created through the canonical schema runner,
//! with the in-process conformance stub as the model (INT-002's offline provider); the
//! attachment test also needs the dev MinIO and is blocked, not skipped, without it.

use std::sync::Arc;
use std::time::Duration;

use axum::http::Request;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use quansio_server::api::seams::QUESTION_SCENARIO;
use quansio_server::api::ApiState;
use quansio_server::artifacts::{ObjectStore, S3ObjectStore};
use quansio_server::control::schema;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0G{n:04}")
}

async fn prepare(
    prefix: &str,
    objects: Option<Arc<dyn ObjectStore>>,
) -> Option<(axum::Router, sqlx::PgPool, String)> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;
    let mut state = ApiState::new(pool.clone()).expect("api");
    if let Some(objects) = objects {
        state = state.with_object_store(objects);
    }
    Some((quansio_server::api::router(state), pool, name))
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
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0GGGGG', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
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

async fn get(router: &axum::Router, path: &str) -> Value {
    let response = router
        .clone()
        .oneshot(http("GET", path, None))
        .await
        .expect("response");
    json_body(response).await
}

#[tokio::test]
async fn a_question_parks_the_run_and_the_answer_in_thread_resumes_it() {
    let Some((router, pool, name)) = prepare("conv_question", None).await else {
        common::blocked_marker();
        return;
    };
    let (status, posted) = command(
        &router,
        "PostMessage",
        1,
        json!({ "workspace_id": WORKSPACE, "content": format!("pick for me {QUESTION_SCENARIO}") }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    assert_eq!(posted["result"]["turn"]["kind"], "parked", "{posted}");
    assert_eq!(
        posted["result"]["turn"]["state"], "WAITING_QUESTION",
        "{posted}"
    );
    let thread_id = posted["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();

    // The question is visible by thread (the desktop asks per thread) and by run.
    let by_thread = get(&router, &format!("/v1/questions?thread_id={thread_id}")).await;
    let questions = by_thread["questions"].as_array().expect("questions");
    assert_eq!(questions.len(), 1, "{by_thread}");
    assert_eq!(questions[0]["status"], "open", "{by_thread}");
    assert_eq!(questions[0]["kind"], "single_choice", "{by_thread}");
    assert_eq!(
        questions[0]["options"],
        json!(["alpha", "beta"]),
        "{by_thread}"
    );
    let question_id = questions[0]["id"].as_str().expect("id").to_string();
    let by_run = get(&router, &format!("/v1/questions?run_id={run_id}")).await;
    assert_eq!(by_run["questions"][0]["id"], question_id, "{by_run}");

    // An answer that is not one of the choices is refused and nothing resumes.
    let (status, refused) = command(
        &router,
        "AnswerQuestion",
        2,
        json!({ "question_id": question_id, "value": "gamma" }),
    )
    .await;
    assert_ne!(status, 200, "{refused}");

    let (status, answered) = command(
        &router,
        "AnswerQuestion",
        3,
        json!({ "question_id": question_id, "value": "alpha" }),
    )
    .await;
    assert_eq!(status, 200, "{answered}");
    assert_eq!(answered["result"]["resumed_run_id"], run_id, "{answered}");
    assert_eq!(
        answered["result"]["turn"]["kind"], "completed",
        "{answered}"
    );
    assert_eq!(answered["result"]["thread_id"], thread_id, "{answered}");
    let reply_id = answered["result"]["reply_message_id"]
        .as_str()
        .expect("a reply message")
        .to_string();

    // The reply is a durable agent message in the same thread, stamped with the run.
    let messages = get(&router, &format!("/v1/messages?thread_id={thread_id}")).await;
    let listed = messages["messages"].as_array().expect("messages");
    assert_eq!(listed.len(), 2, "{messages}");
    assert_eq!(listed[0]["author_kind"], "user", "{messages}");
    assert_eq!(listed[1]["id"], reply_id, "{messages}");
    assert_eq!(listed[1]["author_kind"], "agent", "{messages}");
    assert_eq!(listed[1]["run_id"], run_id, "{messages}");
    assert_eq!(
        listed[1]["content_blocks"][0]["text"], "done: you chose alpha",
        "{messages}"
    );

    // The inspector: two turns, the first parked on a wait step carrying the answered question.
    let timeline = get(&router, &format!("/v1/runs/{run_id}/timeline")).await;
    assert_eq!(timeline["run"]["id"], run_id, "{timeline}");
    assert_eq!(
        timeline["trigger_message"]["thread_id"], thread_id,
        "{timeline}"
    );
    let turns = timeline["turns"].as_array().expect("turns");
    assert_eq!(turns.len(), 2, "{timeline}");
    assert_eq!(turns[0]["input"]["kind"], "message", "{timeline}");
    assert_eq!(turns[1]["input"]["kind"], "wake", "{timeline}");
    let steps = turns[0]["steps"].as_array().expect("steps");
    let model_step = steps
        .iter()
        .find(|step| step["kind"] == "model_call")
        .expect("a model_call step");
    assert!(
        model_step["model_call"]["route"]["provider"].is_string(),
        "the route that answered is part of the inspector: {timeline}"
    );
    let wait_step = steps
        .iter()
        .find(|step| step["kind"] == "wait")
        .expect("a wait step");
    assert_eq!(wait_step["question"]["id"], question_id, "{timeline}");
    assert_eq!(wait_step["question"]["status"], "answered", "{timeline}");
    assert_eq!(
        wait_step["question"]["answer"]["value"], "alpha",
        "{timeline}"
    );
    assert_eq!(
        timeline["questions"].as_array().map(Vec::len),
        Some(1),
        "{timeline}"
    );

    // The thread list shows the thread with both messages.
    let threads = get(&router, &format!("/v1/threads?workspace_id={WORKSPACE}")).await;
    let listed = threads["threads"].as_array().expect("threads");
    assert_eq!(listed.len(), 1, "{threads}");
    assert_eq!(listed[0]["id"], thread_id, "{threads}");
    assert_eq!(listed[0]["message_count"], 2, "{threads}");

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn cancel_turn_stops_a_parked_run_and_withdraws_its_question() {
    let Some((router, pool, name)) = prepare("conv_cancel", None).await else {
        common::blocked_marker();
        return;
    };
    let (status, posted) = command(
        &router,
        "PostMessage",
        1,
        json!({ "workspace_id": WORKSPACE, "content": format!("{QUESTION_SCENARIO} then stop") }),
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
    let question_id = get(&router, &format!("/v1/questions?run_id={run_id}")).await["questions"][0]
        ["id"]
        .as_str()
        .expect("question")
        .to_string();

    let (status, cancelled) = command(&router, "CancelTurn", 2, json!({ "run_id": run_id })).await;
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(cancelled["result"]["status"], "CANCELLED", "{cancelled}");
    assert_eq!(
        cancelled["result"]["cancelled_questions"],
        json!([question_id]),
        "{cancelled}"
    );

    let questions = get(&router, &format!("/v1/questions?thread_id={thread_id}")).await;
    assert_eq!(
        questions["questions"][0]["status"], "cancelled",
        "{questions}"
    );

    // A late answer is refused: the question is no longer open.
    let (status, late) = command(
        &router,
        "AnswerQuestion",
        3,
        json!({ "question_id": question_id, "value": "alpha" }),
    )
    .await;
    assert_eq!(status, 409, "{late}");
    assert_eq!(late["code"], "CONFLICT_STATE", "{late}");

    let run = get(&router, &format!("/v1/runs/{run_id}")).await;
    assert_eq!(run["status"], "CANCELLED", "{run}");

    // Cancelling again is idempotent by the run's own rule: same state, no new event.
    let (status, again) = command(&router, "CancelTurn", 4, json!({ "run_id": run_id })).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["result"]["cancelled_questions"], json!([]), "{again}");

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn an_upload_becomes_an_attachment_the_message_and_the_inspector_carry() {
    let objects: Arc<dyn ObjectStore> =
        Arc::new(S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration"));
    let Some((router, pool, name)) = prepare("conv_attach", Some(objects)).await else {
        common::blocked_marker();
        return;
    };
    // A real binary payload, not text: every byte value appears, and it is not valid UTF-8.
    let bytes: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let (status, created) = command(
        &router,
        "CreateArtifact",
        1,
        json!({
            "workspace_id": WORKSPACE,
            "title": "sample.bin",
            "media_type": "application/octet-stream",
            "bytes_base64": STANDARD.encode(&bytes),
        }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    let artifact_id = created["result"]["artifact_id"]
        .as_str()
        .expect("artifact")
        .to_string();
    assert!(artifact_id.starts_with("art_"), "{created}");
    assert_eq!(created["result"]["size_bytes"], 4096, "{created}");

    // Resending the same command replays the same result rather than a second artifact.
    let (status, replayed) = command(
        &router,
        "CreateArtifact",
        1,
        json!({
            "workspace_id": WORKSPACE,
            "title": "sample.bin",
            "media_type": "application/octet-stream",
            "bytes_base64": STANDARD.encode(&bytes),
        }),
    )
    .await;
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["replayed"], true, "{replayed}");
    assert_eq!(replayed["result"]["artifact_id"], artifact_id, "{replayed}");

    // A message may attach only artifacts of its workspace.
    let (status, refused) = command(
        &router,
        "PostMessage",
        2,
        json!({
            "workspace_id": WORKSPACE,
            "content": "look at this",
            "attachments": ["art_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ"],
        }),
    )
    .await;
    assert_ne!(status, 200, "{refused}");

    let (status, posted) = command(
        &router,
        "PostMessage",
        3,
        json!({
            "workspace_id": WORKSPACE,
            "content": "look at this",
            "attachments": [artifact_id],
        }),
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

    let messages = get(&router, &format!("/v1/messages?thread_id={thread_id}")).await;
    assert_eq!(
        messages["messages"][0]["attachments"],
        json!([artifact_id]),
        "{messages}"
    );
    let timeline = get(&router, &format!("/v1/runs/{run_id}/timeline")).await;
    assert_eq!(
        timeline["trigger_message"]["attachments"],
        json!([artifact_id]),
        "{timeline}"
    );

    let artifacts = get(&router, &format!("/v1/artifacts?workspace_id={WORKSPACE}")).await;
    assert_eq!(artifacts["artifacts"][0]["id"], artifact_id, "{artifacts}");
    assert_eq!(artifacts["artifacts"][0]["size_bytes"], 4096, "{artifacts}");
    let versions = get(&router, &format!("/v1/artifacts/{artifact_id}/versions")).await;
    assert_eq!(
        versions["versions"].as_array().map(Vec::len),
        Some(1),
        "{versions}"
    );
    assert_eq!(
        versions["versions"][0]["content_digest"], created["result"]["content_digest"],
        "{versions}"
    );

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn without_an_object_store_uploads_fail_closed() {
    let Some((router, pool, name)) = prepare("conv_noobj", None).await else {
        common::blocked_marker();
        return;
    };
    let (status, refused) = command(
        &router,
        "CreateArtifact",
        1,
        json!({ "workspace_id": WORKSPACE, "title": "x", "bytes_base64": "AAAA" }),
    )
    .await;
    assert_eq!(status, 503, "{refused}");
    assert_eq!(refused["code"], "TARGET_UNAVAILABLE", "{refused}");
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn live_frames_reach_a_workspace_subscriber_while_the_turn_runs() {
    let Some((router, pool, name)) = prepare("conv_live", None).await else {
        common::blocked_marker();
        return;
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut request = format!("ws://{addr}/v1/stream")
        .into_client_request()
        .expect("ws request");
    request
        .headers_mut()
        .insert("x-quansio-tenant", TENANT.parse().expect("header"));
    // The subscriber is the person who posts: the thread their message opens is protected
    // (APP-009), so its live frames reach its participants and nobody else.
    request
        .headers_mut()
        .insert("x-quansio-user", USER.parse().expect("header"));
    request
        .headers_mut()
        .insert("x-quansio-consumer", "live".parse().expect("header"));
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect");
    ws.send(Message::Text(
        json!({ "subscribe": [{ "channel": format!("workspace:{WORKSPACE}") }] }).to_string(),
    ))
    .await
    .expect("subscribe");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let posted = reqwest::Client::new()
        .post(format!("http://{addr}/v1/commands/PostMessage"))
        .header("x-quansio-tenant", TENANT)
        .header("x-quansio-user", USER)
        .header("content-type", "application/json")
        .body(
            json!({
                "command_id": command_id(1),
                "params": { "workspace_id": WORKSPACE, "content": "stream me" }
            })
            .to_string(),
        )
        .send()
        .await
        .expect("post");
    assert!(posted.status().is_success(), "{}", posted.status());

    // Live frames (transient) and events (durable) arrive on the same socket; the turn's
    // progress and model delta must be among them.
    let mut kinds = Vec::new();
    let mut saw_progress = false;
    let mut saw_delta = false;
    let mut saw_event = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(saw_progress && saw_delta && saw_event) {
        let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout_at(deadline, ws.next()).await
        else {
            break;
        };
        let frame: Value = serde_json::from_str(&text).expect("json frame");
        match frame["kind"].as_str() {
            Some("live") => {
                assert_eq!(
                    frame["channel"],
                    format!("workspace:{WORKSPACE}"),
                    "{frame}"
                );
                let kind = frame["frame"]["kind"].as_str().unwrap_or("").to_string();
                saw_progress |= kind == "progress";
                saw_delta |= kind == "model.delta";
                kinds.push(kind);
            }
            Some("event") => {
                saw_event = true;
                kinds.push(format!("event:{}", frame["event"]["event_type"]));
            }
            _ => kinds.push(format!("?{frame}")),
        }
    }
    assert!(
        saw_progress && saw_delta && saw_event,
        "frames seen: {kinds:?}"
    );
    common::drop_pool(&pool, &name).await;
}

/// A model that takes its time: the turn outlasts the command that started it.
struct SlowModel;

#[async_trait::async_trait]
impl quansio_server::runtime::state_machine::ModelProposalSource for SlowModel {
    async fn propose(
        &self,
        _request: quansio_server::runtime::state_machine::ModelCallRequest,
    ) -> Result<
        quansio_server::runtime::state_machine::ModelProposal,
        quansio_server::runtime::state_machine::RuntimeError,
    > {
        tokio::time::sleep(Duration::from_millis(800)).await;
        Ok(quansio_server::runtime::state_machine::ModelProposal {
            assistant_text: Some("all done".to_string()),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn served_a_message_is_accepted_before_its_turn_and_the_reply_follows() {
    let name = common::scratch_name("conv_accepted");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    seed_policy(&pool).await;
    // The served composition: commands answer once accepted, turns run after them.
    let router = quansio_server::api::router(
        ApiState::new(pool.clone())
            .expect("api")
            .with_model_source(Arc::new(SlowModel))
            .with_background_turns(),
    );
    let params = json!({ "workspace_id": WORKSPACE, "content": "Summarize the week" });
    let started = std::time::Instant::now();
    let (status, posted) = command(&router, "PostMessage", 1, params.clone()).await;
    let took = started.elapsed();
    assert_eq!(status, 200, "{posted}");
    assert!(
        took < Duration::from_millis(600),
        "the command waited {took:?} for an 800 ms turn instead of answering once accepted"
    );
    assert!(posted["accepted_at"].is_string(), "{posted}");
    assert_eq!(posted["result"]["run_status"], "RUNNING", "{posted}");
    assert!(
        posted["result"].get("reply_message_id").is_none(),
        "{posted}"
    );
    let thread = posted["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();

    // The reply follows the command, as the turn finishes.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let messages = get(&router, &format!("/v1/messages?thread_id={thread}")).await;
        if messages.to_string().contains("all done") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no reply: {messages}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A resend is the same accepted command.
    let (status, replay) = command(&router, "PostMessage", 1, params).await;
    assert!(status == 200 || status == 201, "{replay}");
    assert_eq!(replay["replayed"], true, "{replay}");
    assert_eq!(replay["accepted_at"], posted["accepted_at"], "{replay}");
    common::drop_pool(&pool, &name).await;
}
