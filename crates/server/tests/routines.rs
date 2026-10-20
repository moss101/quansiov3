//! APP-010: routines, schedules, events and notifications, against a scratch database
//! (blocked, not skipped, without one).
//!
//! * schedule restart — a due window fires once; a second scheduler instance, a restart
//!   after the claim, a crash after the run was created, an expired lease and two
//!   concurrent instances all leave exactly one run per window;
//! * the routine's run takes the interactive path (the teammate, the capability
//!   projection, the turn and the reply of a posted message);
//! * event dedupe — a matching event claims one fire however often or concurrently it is
//!   consumed, and a routine's own activity never triggers it;
//! * notification preferences — the channels a person chose decide what is made and
//!   delivered; email is retried with backoff; the same event notifies once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::Request;
use chrono::{Duration as ChronoDuration, Utc};
use quansio_events::EventStore;
use quansio_server::api::routines::InteractiveDispatch;
use quansio_server::api::ApiState;
use quansio_server::control::collaboration::ThreadGate;
use quansio_server::control::routines::{EventTriggers, FireLedger};
use quansio_server::control::schema;
use quansio_server::notify::{EmailDeliveries, EmailMessage, EmailTransport, Notifier};
use quansio_server::scheduler::{
    DispatchOutcome, RoutineFireOutcome, RoutineFireRequest, RoutineScheduler, RunDispatch,
    RunDispatchRequest, RunResumeRequest, SchedulerError,
};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0R0TNE";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0R0TN1";
const EDITOR: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0R0TN2";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0R0TN3";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0R0TNE";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0R0TNE";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0R{n:04}")
}

struct Harness {
    router: axum::Router,
    state: ApiState,
    pool: sqlx::PgPool,
    name: String,
}

async fn prepare(prefix: &str) -> Option<Harness> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    for (user, role) in [(EDITOR, "editor"), (VIEWER, "viewer")] {
        sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, $3)")
            .bind(user)
            .bind(format!("{user}@example.com"))
            .bind(user)
            .execute(&mut *tx)
            .await
            .expect("user");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, 'member', 'active')")
            .bind(format!("tm_{}", &user[4..]))
            .bind(TENANT)
            .bind(user)
            .execute(&mut *tx)
            .await
            .expect("tenant membership");
        sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ($1, $2, $3, $4, $5, 'active')")
            .bind(format!("wm_{}", &user[4..]))
            .bind(TENANT)
            .bind(WORKSPACE)
            .bind(user)
            .bind(role)
            .execute(&mut *tx)
            .await
            .expect("workspace membership");
    }
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0R0TNE', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([{ "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    let state = ApiState::new(pool.clone()).expect("api");
    Some(Harness {
        router: quansio_server::api::router(state.clone()),
        state,
        pool,
        name,
    })
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn command(
    router: &axum::Router,
    user: &str,
    name: &str,
    id: u32,
    params: Value,
) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(http(
            "POST",
            &format!("/v1/commands/{name}"),
            user,
            Some(json!({ "command_id": command_id(id), "params": params })),
        ))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn get(router: &axum::Router, user: &str, path: &str) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(http("GET", path, user, None))
        .await
        .expect("response");
    let status = response.status().as_u16();
    (status, json_body(response).await)
}

async fn scalar_i64(pool: &sqlx::PgPool, sql: &str, bind: &str) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let value: i64 = sqlx::query_scalar(sql)
        .bind(TENANT)
        .bind(bind)
        .fetch_one(&mut *tx)
        .await
        .expect("scalar");
    tx.commit().await.expect("commit");
    value
}

async fn exec(pool: &sqlx::PgPool, sql: &str, bind: &str) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(sql)
        .bind(TENANT)
        .bind(bind)
        .execute(&mut *tx)
        .await
        .expect("exec");
    tx.commit().await.expect("commit");
}

async fn create_routine(h: &Harness, id: u32, trigger: Value) -> String {
    let (status, created) = command(
        &h.router,
        OWNER,
        "CreateRoutine",
        id,
        json!({ "workspace_id": WORKSPACE, "title": "Daily digest", "prompt": "summarize the day", "trigger": trigger, "absence_policy": "catch_up_once" }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    created["result"]["id"].as_str().expect("rtn").to_string()
}

/// Make a schedule routine due now: its next window is moved `minutes_ago` into the past.
/// Each window of a test is a distinct instant — the same instant *is* the same window, and
/// the ledger (rightly) fires it once.
async fn make_due(h: &Harness, routine_id: &str, minutes_ago: i32) -> chrono::DateTime<Utc> {
    exec(
        &h.pool,
        &format!(
            "UPDATE routines SET next_due_at = date_trunc('second', now()) - interval '{minutes_ago} minutes' \
             WHERE tenant_id = $1 AND id = $2"
        ),
        routine_id,
    )
    .await;
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let due: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT next_due_at FROM routines WHERE tenant_id = $1 AND id = $2")
            .bind(TENANT)
            .bind(routine_id)
            .fetch_one(&mut *tx)
            .await
            .expect("due");
    tx.commit().await.expect("commit");
    due
}

async fn runs_of(h: &Harness, routine_id: &str) -> i64 {
    scalar_i64(
        &h.pool,
        "SELECT count(*) FROM runs r JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
         WHERE r.tenant_id = $1 AND r.trigger_kind = 'routine' AND m.author_id = 'routine:' || $2",
        routine_id,
    )
    .await
}

/// A dispatch that fails before doing anything, or after doing the whole fire (a crash
/// after the run exists and before the ledger heard of it).
struct Faulty {
    inner: InteractiveDispatch,
    after: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl RunDispatch for Faulty {
    async fn dispatch_queued(
        &self,
        request: RunDispatchRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        self.inner.dispatch_queued(request).await
    }
    async fn resume_timer_wait(
        &self,
        request: RunResumeRequest,
    ) -> Result<DispatchOutcome, SchedulerError> {
        self.inner.resume_timer_wait(request).await
    }
    async fn fire_routine(
        &self,
        request: RoutineFireRequest,
    ) -> Result<RoutineFireOutcome, SchedulerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.after {
            self.inner.fire_routine(request.clone()).await?;
        }
        Err(SchedulerError::RunDispatch {
            run_id: request.routine_id,
            reason: "the executor died".to_string(),
        })
    }
}

#[tokio::test]
async fn routine_commands_validate_and_transition() {
    let Some(h) = prepare("routine_commands").await else {
        common::blocked_marker();
        return;
    };
    let routine = create_routine(&h, 1, json!({ "kind": "interval", "spec": "1h" })).await;
    let (_, list) = get(
        &h.router,
        EDITOR,
        &format!("/v1/routines?workspace_id={WORKSPACE}"),
    )
    .await;
    let item = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|r| r["id"] == routine.as_str())
        .cloned()
        .expect("routine");
    assert_eq!(item["status"], "active");
    assert!(item["next_due_at"].is_string(), "{item}");
    assert_eq!(item["owner_user_id"], OWNER);

    for (n, params) in [
        (
            2,
            json!({ "workspace_id": WORKSPACE, "title": "x", "prompt": "y", "trigger": { "kind": "cron", "spec": "not a cron" } }),
        ),
        (
            3,
            json!({ "workspace_id": WORKSPACE, "title": "", "prompt": "y", "trigger": { "kind": "interval", "spec": "1h" } }),
        ),
        (
            4,
            json!({ "workspace_id": WORKSPACE, "title": "x", "prompt": "y", "trigger": { "kind": "event", "spec": "routine.fired" } }),
        ),
        (
            5,
            json!({ "workspace_id": WORKSPACE, "title": "x", "prompt": "y", "trigger": { "kind": "interval", "spec": "1h" }, "absence_policy": "sometimes" }),
        ),
    ] {
        let (status, refused) = command(&h.router, OWNER, "CreateRoutine", n, params).await;
        assert_eq!(status, 400, "{refused}");
        assert_eq!(refused["code"], "VALIDATION_SCHEMA");
    }
    let (status, refused) = command(&h.router, VIEWER, "CreateRoutine", 6, json!({ "workspace_id": WORKSPACE, "title": "x", "prompt": "y", "trigger": { "kind": "interval", "spec": "1h" } })).await;
    assert_eq!(status, 403, "{refused}");
    // Only its owner (or an admin) changes it.
    let (status, _) = command(
        &h.router,
        EDITOR,
        "PauseRoutine",
        7,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 403);

    let (status, paused) = command(
        &h.router,
        OWNER,
        "PauseRoutine",
        8,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 200, "{paused}");
    assert_eq!(paused["result"]["status"], "paused");
    let (status, again) = command(
        &h.router,
        OWNER,
        "PauseRoutine",
        9,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    let (status, resumed) = command(
        &h.router,
        OWNER,
        "ResumeRoutine",
        10,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 200, "{resumed}");
    assert_eq!(resumed["result"]["status"], "active");
    let (status, updated) = command(&h.router, OWNER, "UpdateRoutine", 11, json!({ "routine_id": routine, "trigger": { "kind": "cron", "spec": "0 9 * * *", "timezone": "+02:00" }, "prompt": "summarize the week" })).await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(
        updated["result"]["objective_template"]["prompt"],
        "summarize the week"
    );
    assert_eq!(
        updated["result"]["objective_template"]["title"],
        "Daily digest"
    );
    let (status, deleted) = command(
        &h.router,
        OWNER,
        "DeleteRoutine",
        12,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["result"]["next_due_at"], Value::Null);
    let (status, _) = command(
        &h.router,
        OWNER,
        "TriggerRoutineNow",
        13,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 409, "a deleted routine does not fire");
    let (_, list) = get(
        &h.router,
        OWNER,
        &format!("/v1/routines?workspace_id={WORKSPACE}"),
    )
    .await;
    assert!(!list["items"]
        .as_array()
        .expect("items")
        .iter()
        .any(|r| r["id"] == routine.as_str()));
    let types: Vec<String> = EventStore::new(h.pool.clone())
        .events_for_aggregate(TENANT, "routine", &routine)
        .await
        .expect("events")
        .iter()
        .map(|event| event.event_type.to_string())
        .collect();
    assert_eq!(
        types,
        [
            "routine.created",
            "routine.paused",
            "routine.resumed",
            "routine.updated",
            "routine.deleted"
        ]
    );
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_scheduled_window_fires_once_across_restarts() {
    let Some(h) = prepare("routine_restart").await else {
        common::blocked_marker();
        return;
    };
    let routine = create_routine(&h, 1, json!({ "kind": "interval", "spec": "1h" })).await;
    let window = make_due(&h, &routine, 1).await;
    let dispatch = InteractiveDispatch::new(h.state.clone());

    // The window fires once, through the interactive path.
    let first = RoutineScheduler::new(h.pool.clone(), TENANT).expect("scheduler");
    let report = first.tick(&dispatch, Utc::now(), 16).await.expect("tick");
    assert_eq!((report.fired, report.executed), (1, 1), "{report:?}");
    assert_eq!(runs_of(&h, &routine).await, 1);
    let ledger = FireLedger::new(h.pool.clone(), TENANT);
    let fires = ledger.fires_of(&routine, 10).await.expect("fires");
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].status.as_str(), "fired");
    assert_eq!(
        fires[0].fire_key,
        format!("routine:{routine}:{}", window.to_rfc3339())
    );
    let run_id = fires[0].run_id.clone().expect("run");
    let (_, run) = get(&h.router, OWNER, &format!("/v1/runs/{run_id}")).await;
    assert!(
        run["capability_projection"]["grants"].is_array(),
        "the routine's run has the interactive projection: {run}"
    );
    let thread = fires[0].thread_id.clone().expect("thread");
    let (_, messages) = get(
        &h.router,
        OWNER,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    let messages = messages["messages"].as_array().expect("messages").clone();
    assert_eq!(messages[0]["author_id"], format!("routine:{routine}"));
    assert_eq!(
        messages[0]["content_blocks"][1]["text"],
        "summarize the day"
    );
    assert!(
        messages
            .iter()
            .any(|m| m["author_kind"] == "agent" && m["run_id"] == run_id.as_str()),
        "the teammate's reply is posted: {messages:?}"
    );

    // A restart: a new instance finds nothing due and fires nothing.
    let second = RoutineScheduler::new(h.pool.clone(), TENANT).expect("scheduler");
    let report = second.tick(&dispatch, Utc::now(), 16).await.expect("tick");
    assert_eq!((report.fired, report.executed), (0, 0));
    assert_eq!(runs_of(&h, &routine).await, 1);

    // A crash before execution: the claim stands, the retry fires it once.
    make_due(&h, &routine, 2).await;
    let dead = Faulty {
        inner: InteractiveDispatch::new(h.state.clone()),
        after: false,
        calls: AtomicUsize::new(0),
    };
    let report = second.tick(&dead, Utc::now(), 16).await.expect("tick");
    assert_eq!((report.fired, report.execution_failures), (1, 1));
    assert_eq!(runs_of(&h, &routine).await, 1, "nothing ran yet");
    let pending = ledger.fires_of(&routine, 10).await.expect("fires");
    assert_eq!(pending[0].status.as_str(), "pending");
    assert!(
        pending[0].next_attempt_at > Utc::now(),
        "retried after backoff"
    );
    let report = second.tick(&dispatch, Utc::now(), 16).await.expect("tick");
    assert_eq!(report.executed, 0, "not before the backoff");
    let (executed, _) = second
        .execute_pending(&dispatch, Utc::now() + ChronoDuration::minutes(5), 16)
        .await
        .expect("execute");
    assert_eq!(executed, 1);
    assert_eq!(runs_of(&h, &routine).await, 2, "one run per window");

    // A crash after the run was made and before the ledger knew: the retry finds the run.
    make_due(&h, &routine, 3).await;
    let late = Faulty {
        inner: InteractiveDispatch::new(h.state.clone()),
        after: true,
        calls: AtomicUsize::new(0),
    };
    second.tick(&late, Utc::now(), 16).await.expect("tick");
    assert_eq!(runs_of(&h, &routine).await, 3);
    let recorded = ledger.fires_of(&routine, 1).await.expect("fires")[0].clone();
    assert!(
        recorded.run_id.is_some(),
        "the steps were recorded as they happened"
    );
    second
        .execute_pending(&dispatch, Utc::now() + ChronoDuration::minutes(5), 16)
        .await
        .expect("execute");
    assert_eq!(
        runs_of(&h, &routine).await,
        3,
        "the resumed fire reused its run"
    );
    let done = ledger.fires_of(&routine, 1).await.expect("fires")[0].clone();
    assert_eq!(done.status.as_str(), "fired");
    assert_eq!(done.run_id, recorded.run_id);

    // An executor that died holding the lease: once the lease expires, the fire runs once.
    make_due(&h, &routine, 4).await;
    let never = Faulty {
        inner: InteractiveDispatch::new(h.state.clone()),
        after: false,
        calls: AtomicUsize::new(0),
    };
    second.tick(&never, Utc::now(), 16).await.expect("tick");
    exec(&h.pool, "UPDATE routine_fires SET status = 'running', lease_until = now() - interval '1 second', next_attempt_at = now() WHERE tenant_id = $1 AND routine_id = $2 AND status = 'pending'", &routine).await;
    second
        .execute_pending(&dispatch, Utc::now(), 16)
        .await
        .expect("execute");
    assert_eq!(runs_of(&h, &routine).await, 4);

    // Two instances racing on one due window claim it once.
    make_due(&h, &routine, 5).await;
    let a = RoutineScheduler::new(h.pool.clone(), TENANT).expect("a");
    let b = RoutineScheduler::new(h.pool.clone(), TENANT).expect("b");
    let (ra, rb) = tokio::join!(
        a.tick(&dispatch, Utc::now(), 16),
        b.tick(&dispatch, Utc::now(), 16)
    );
    assert_eq!(ra.expect("a").fired + rb.expect("b").fired, 1);
    assert_eq!(runs_of(&h, &routine).await, 5);
    let fired = scalar_i64(
        &h.pool,
        "SELECT count(*) FROM routine_fires WHERE tenant_id = $1 AND routine_id = $2",
        &routine,
    )
    .await;
    assert_eq!(fired, 5, "five windows, five fires");
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn an_event_fires_its_routine_once_however_it_is_consumed() {
    let Some(h) = prepare("routine_events").await else {
        common::blocked_marker();
        return;
    };
    let routine = create_routine(&h, 1, json!({ "kind": "event", "spec": "thread.created" })).await;
    // An event from before the routine existed never triggers it.
    let triggers = EventTriggers::new(h.pool.clone(), TENANT);
    triggers.scan(1000).await.expect("scan");
    assert_eq!(runs_of(&h, &routine).await, 0);
    let (status, _) = command(
        &h.router,
        OWNER,
        "CreateThread",
        2,
        json!({ "workspace_id": WORKSPACE, "kind": "group", "title": "planning" }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(triggers.scan(1000).await.expect("scan"), 1);
    assert_eq!(
        triggers.scan(1000).await.expect("scan"),
        0,
        "the cursor moved past it"
    );
    // Replaying the log from the start (the cursor lost) claims nothing new.
    exec(
        &h.pool,
        "DELETE FROM event_cursors WHERE tenant_id = $1 AND consumer = $2",
        "routine-event-triggers",
    )
    .await;
    assert_eq!(triggers.scan(1000).await.expect("scan"), 0);
    // Two consumers at once on a new event: one fire.
    let (status, _) = command(
        &h.router,
        OWNER,
        "CreateThread",
        3,
        json!({ "workspace_id": WORKSPACE, "kind": "group", "title": "launch" }),
    )
    .await;
    assert_eq!(status, 200);
    let other = EventTriggers::new(h.pool.clone(), TENANT);
    let (x, y) = tokio::join!(triggers.scan(1000), other.scan(1000));
    assert_eq!(x.expect("x") + y.expect("y"), 1);
    let fires = scalar_i64(
        &h.pool,
        "SELECT count(*) FROM routine_fires WHERE tenant_id = $1 AND routine_id = $2",
        &routine,
    )
    .await;
    assert_eq!(fires, 2, "one fire per event");

    // Executing the fires runs the routine; what it does does not trigger it again.
    let scheduler = RoutineScheduler::new(h.pool.clone(), TENANT).expect("scheduler");
    let dispatch = InteractiveDispatch::new(h.state.clone());
    let (executed, _) = scheduler
        .execute_pending(&dispatch, Utc::now(), 16)
        .await
        .expect("execute");
    assert_eq!(executed, 2);
    assert_eq!(runs_of(&h, &routine).await, 2);
    assert_eq!(
        triggers.scan(1000).await.expect("scan"),
        0,
        "a routine never triggers itself"
    );
    common::drop_pool(&h.pool, &h.name).await;
}

#[derive(Default)]
struct Capture {
    sent: Mutex<Vec<EmailMessage>>,
    fail: bool,
}

#[async_trait]
impl EmailTransport for Capture {
    async fn send(&self, message: &EmailMessage) -> Result<(), String> {
        if self.fail {
            return Err("the mail API answered 503".to_string());
        }
        self.sent.lock().expect("sent").push(message.clone());
        Ok(())
    }
}

#[tokio::test]
async fn notifications_follow_preferences_and_deliver_email() {
    let Some(h) = prepare("routine_notify").await else {
        common::blocked_marker();
        return;
    };
    let (status, thread) = command(&h.router, OWNER, "CreateThread", 1, json!({ "workspace_id": WORKSPACE, "kind": "group", "title": "launch", "participants": [{ "kind": "user", "id": EDITOR }] })).await;
    assert_eq!(status, 200, "{thread}");
    let thread = thread["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let mention = |n: u32| {
        let router = h.router.clone();
        let thread = thread.clone();
        async move {
            let (status, body) = command(&router, OWNER, "PostMessage", n, json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "@editor look", "mentions": [{ "kind": "user", "id": EDITOR }] })).await;
            assert_eq!(status, 200, "{body}");
        }
    };
    let notifier = Notifier::new(h.pool.clone(), TENANT);
    let notifications = |user: &'static str| {
        let router = h.router.clone();
        async move {
            get(
                &router,
                user,
                &format!("/v1/notifications?workspace_id={WORKSPACE}"),
            )
            .await
            .1
        }
    };

    // In-app only.
    let (status, pref) = command(
        &h.router,
        EDITOR,
        "UpdateNotificationPreferences",
        2,
        json!({ "workspace_id": WORKSPACE, "kind": "mention", "channels": ["in_app"] }),
    )
    .await;
    assert_eq!(status, 200, "{pref}");
    mention(3).await;
    notifier.scan(1000).await.expect("scan");
    let view = notifications(EDITOR).await;
    let first = view["items"].as_array().expect("items")[0].clone();
    assert_eq!(first["kind"], "mention");
    assert_eq!(first["channels"], json!(["in_app"]));
    assert_eq!(first["channels_delivered"], json!(["in_app"]));
    assert_eq!(view["unread"], 1);
    let chosen = view["preferences"]
        .as_array()
        .expect("prefs")
        .iter()
        .find(|p| p["kind"] == "mention")
        .cloned()
        .expect("mention");
    assert_eq!(chosen["chosen"], true);
    // The owner sees none of the editor's notifications, on the projection or the stream.
    assert_eq!(notifications(OWNER).await["items"], json!([]));
    let created: Vec<quansio_events::RuntimeEvent> = EventStore::new(h.pool.clone())
        .read_events_after(TENANT, None, 10_000)
        .await
        .expect("events")
        .into_iter()
        .filter(|event| event.event_type.to_string() == "notification.created")
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].payload["desktop"], false);
    assert!(
        ThreadGate::new(h.pool.clone(), TENANT, Some(EDITOR))
            .admits_event(&created[0])
            .await
    );
    assert!(
        !ThreadGate::new(h.pool.clone(), TENANT, Some(OWNER))
            .admits_event(&created[0])
            .await
    );
    // Consuming the same events again notifies nobody twice.
    exec(
        &h.pool,
        "DELETE FROM event_cursors WHERE tenant_id = $1 AND consumer = $2",
        "notifier",
    )
    .await;
    assert_eq!(notifier.scan(1000).await.expect("rescan"), 0);

    // Desktop and email.
    command(&h.router, EDITOR, "UpdateNotificationPreferences", 4, json!({ "workspace_id": WORKSPACE, "kind": "mention", "channels": ["in_app", "desktop", "email"] })).await;
    mention(5).await;
    notifier.scan(1000).await.expect("scan");
    let capture = Arc::new(Capture::default());
    let deliveries = EmailDeliveries::new(
        h.pool.clone(),
        TENANT,
        Some(capture.clone() as Arc<dyn EmailTransport>),
    );
    let report = deliveries
        .deliver_due(Utc::now(), 64)
        .await
        .expect("deliver");
    assert_eq!(report.delivered, 1, "{report:?}");
    let sent = capture.sent.lock().expect("sent").clone();
    assert_eq!(sent[0].to, format!("{EDITOR}@example.com"));
    assert!(sent[0].subject.contains("mentioned"), "{}", sent[0].subject);
    let view = notifications(EDITOR).await;
    let latest = view["items"].as_array().expect("items")[0].clone();
    assert_eq!(
        latest["channels_delivered"],
        json!(["in_app", "desktop", "email"])
    );

    // A failing provider is retried with backoff, then given up on.
    mention(6).await;
    notifier.scan(1000).await.expect("scan");
    let failing = EmailDeliveries::new(
        h.pool.clone(),
        TENANT,
        Some(Arc::new(Capture {
            fail: true,
            ..Capture::default()
        }) as Arc<dyn EmailTransport>),
    );
    let mut now = Utc::now();
    let mut retried = 0;
    let mut failed = 0;
    for _ in 0..6 {
        let report = failing.deliver_due(now, 64).await.expect("deliver");
        retried += report.retried;
        failed += report.failed;
        now += ChronoDuration::hours(2);
    }
    assert_eq!((retried, failed), (4, 1));
    // Without a transport the delivery is recorded unavailable, not dropped.
    mention(7).await;
    notifier.scan(1000).await.expect("scan");
    let none = EmailDeliveries::new(h.pool.clone(), TENANT, None);
    assert_eq!(
        none.deliver_due(Utc::now(), 64)
            .await
            .expect("deliver")
            .unavailable,
        1
    );

    // Muted: nothing is made.
    command(
        &h.router,
        EDITOR,
        "UpdateNotificationPreferences",
        8,
        json!({ "workspace_id": WORKSPACE, "kind": "mention", "channels": [] }),
    )
    .await;
    let before = notifications(EDITOR).await["items"]
        .as_array()
        .map_or(0, Vec::len);
    mention(9).await;
    notifier.scan(1000).await.expect("scan");
    assert_eq!(
        notifications(EDITOR).await["items"]
            .as_array()
            .map_or(0, Vec::len),
        before
    );
    let (status, bad) = command(
        &h.router,
        EDITOR,
        "UpdateNotificationPreferences",
        10,
        json!({ "workspace_id": WORKSPACE, "kind": "mention", "channels": ["sms"] }),
    )
    .await;
    assert_eq!(status, 400, "{bad}");

    // Read.
    let id = first["id"].as_str().expect("id").to_string();
    let (status, read) = command(
        &h.router,
        EDITOR,
        "MarkNotificationRead",
        11,
        json!({ "notification_id": id }),
    )
    .await;
    assert_eq!(status, 200, "{read}");
    assert!(read["result"]["read_at"].is_string());
    let (status, _) = command(
        &h.router,
        OWNER,
        "MarkNotificationRead",
        12,
        json!({ "notification_id": id }),
    )
    .await;
    assert_eq!(status, 404, "another person's notification");

    // A routine's outcome reaches its owner.
    let routine = create_routine(&h, 13, json!({ "kind": "interval", "spec": "1h" })).await;
    let (status, fired) = command(
        &h.router,
        OWNER,
        "TriggerRoutineNow",
        14,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 200, "{fired}");
    assert_eq!(fired["result"]["status"], "fired", "{fired}");
    let (status, again) = command(
        &h.router,
        OWNER,
        "TriggerRoutineNow",
        14,
        json!({ "routine_id": routine }),
    )
    .await;
    assert_eq!(status, 200, "a retried command replays: {again}");
    assert_eq!(runs_of(&h, &routine).await, 1);
    notifier.scan(1000).await.expect("scan");
    let owner = notifications(OWNER).await;
    assert!(
        owner["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|n| n["kind"] == "completed" && n["ref"]["routine_id"] == routine.as_str()),
        "{owner}"
    );
    common::drop_pool(&h.pool, &h.name).await;
}

/// The effect classes of the capability projection of the routine's runs, one entry per run.
async fn projected_classes(h: &Harness, routine_id: &str) -> Vec<Vec<String>> {
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    let grants: Vec<Value> = sqlx::query_scalar(
        "SELECT p.grants FROM capability_projections p \
         JOIN runs r ON r.tenant_id = p.tenant_id AND r.id = p.subject_id AND p.subject_kind = 'run' \
         JOIN messages m ON m.tenant_id = r.tenant_id AND m.id = r.trigger_ref \
         WHERE r.tenant_id = $1 AND m.author_id = 'routine:' || $2 ORDER BY r.created_at",
    )
    .bind(TENANT)
    .bind(routine_id)
    .fetch_all(&mut *tx)
    .await
    .expect("projections");
    tx.commit().await.expect("commit");
    grants
        .iter()
        .map(|run| {
            run.as_array()
                .expect("grants")
                .iter()
                .map(|grant| grant["effect_class"].as_str().expect("class").to_string())
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn an_observing_routine_s_runs_can_read_and_report_and_nothing_else() {
    let Some(h) = prepare("routine_observe").await else {
        common::blocked_marker();
        return;
    };
    // A workspace policy that lets agents write: the authority an acting routine's run is given.
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query("UPDATE policies SET rules = $2 WHERE tenant_id = $1 AND scope = 'tenant'")
        .bind(TENANT)
        .bind(json!([
            { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
            { "effect_class": "read.external", "resource": { "kind": "domain", "selector": "*" }, "decision": "allow" },
            { "effect_class": "fs.write.workspace", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
            { "effect_class": "message.send", "resource": { "kind": "domain", "selector": "*" }, "decision": "ask" }
        ]))
        .execute(&mut *tx)
        .await
        .expect("policy");
    tx.commit().await.expect("commit");

    // Anything but `act` and `observe` is refused.
    let (status, refused) = command(&h.router, OWNER, "CreateRoutine", 1, json!({ "workspace_id": WORKSPACE, "title": "x", "prompt": "p", "mode": "autonomous", "trigger": { "kind": "interval", "spec": "1h" } })).await;
    assert_eq!(status, 400, "{refused}");

    let create = |id: u32, title: &str, mode: Option<&str>| {
        let mut params = json!({ "workspace_id": WORKSPACE, "title": title, "prompt": "look at the board and report", "trigger": { "kind": "interval", "spec": "1h" } });
        if let Some(mode) = mode {
            params["mode"] = json!(mode);
        }
        let (router,) = (h.router.clone(),);
        async move { command(&router, OWNER, "CreateRoutine", id, params).await }
    };
    let (status, watching) = create(2, "Watcher", Some("observe")).await;
    assert_eq!(status, 200, "{watching}");
    assert_eq!(watching["result"]["objective_template"]["mode"], "observe");
    let (status, working) = create(3, "Worker", None).await;
    assert_eq!(status, 200, "{working}");
    assert!(
        working["result"]["objective_template"]
            .get("mode")
            .is_none(),
        "act is the default and is not stored"
    );
    let (watching, working) = (
        watching["result"]["id"].as_str().unwrap().to_string(),
        working["result"]["id"].as_str().unwrap().to_string(),
    );

    for (n, routine) in [(4, &watching), (5, &working)] {
        let (status, fired) = command(
            &h.router,
            OWNER,
            "TriggerRoutineNow",
            n,
            json!({ "routine_id": routine }),
        )
        .await;
        assert_eq!(status, 200, "{fired}");
    }
    let observed = projected_classes(&h, &watching).await;
    let acted = projected_classes(&h, &working).await;
    assert_eq!(observed.len(), 1);
    assert!(
        observed[0].iter().all(|class| class.starts_with("read.")) && !observed[0].is_empty(),
        "an observing run was given {:?}",
        observed[0]
    );
    // The control: the same workspace gives an acting routine's run the writes the policy allows.
    assert!(
        acted[0].iter().any(|class| class == "fs.write.workspace")
            && acted[0].iter().any(|class| class == "message.send"),
        "an acting run was given {:?}",
        acted[0]
    );

    // Updating it keeps the mode unless the caller says; saying `act` clears it, and it is the owner's call to make.
    let (status, kept) = command(
        &h.router,
        OWNER,
        "UpdateRoutine",
        6,
        json!({ "routine_id": watching, "prompt": "look again" }),
    )
    .await;
    assert_eq!(status, 200, "{kept}");
    assert_eq!(kept["result"]["objective_template"]["mode"], "observe");
    // A full replacement that leaves the mode out does not lose it.
    let revision = kept["result"]["revision"].as_i64().expect("revision");
    let (status, replaced) = command(
        &h.router,
        OWNER,
        "UpdateRoutine",
        8,
        json!({ "routine_id": watching, "expected_revision": revision, "title": "Watcher", "prompt": "look once more", "trigger": { "kind": "interval", "spec": "1h" } }),
    )
    .await;
    assert_eq!(status, 200, "{replaced}");
    assert_eq!(
        replaced["result"]["objective_template"]["mode"], "observe",
        "{replaced}"
    );
    let (status, cleared) = command(
        &h.router,
        OWNER,
        "UpdateRoutine",
        7,
        json!({ "routine_id": watching, "mode": "act" }),
    )
    .await;
    assert_eq!(status, 200, "{cleared}");
    assert!(
        cleared["result"]["objective_template"]
            .get("mode")
            .is_none(),
        "{cleared}"
    );
    common::drop_pool(&h.pool, &h.name).await;
}
