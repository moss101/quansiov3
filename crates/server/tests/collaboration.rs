//! APP-009: collaboration on canonical threads, against a scratch database (blocked, not
//! skipped, without one).
//!
//! * multi-client ordering — concurrent posts from two people get distinct, gap-free `seq`
//!   and their `thread.message_posted` events commit in that order;
//! * membership revoke — a removed participant cannot read the thread, post to it, or
//!   receive its new `thread.*` events or live frames (the stream's [`ThreadGate`]), while
//!   hearing their own removal;
//! * handoff — the responsible teammate moves, a notice is posted, the target's mailbox
//!   receives the handoff, un-addressed messages route to the target, and a handoff is
//!   refused while the source still has a live run in the thread;
//! * the membership rules of DOMAIN.md §3.1, reactions, mentions and derived attention.

use axum::http::Request;
use quansio_events::{EventStore, LiveFrame, RuntimeEvent};
use quansio_server::api::ApiState;
use quansio_server::control::collaboration::{thread_access, thread_access_many, ThreadGate};
use quansio_server::control::schema;
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0C0EAB";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0C0EA1";
const EDITOR: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0C0EA2";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0C0EA3";
const OUTSIDER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0C0EA4";
const STRANGER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0C0EA5";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0C0EAB";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0C0EAB";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0C{n:04}")
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
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    for (user, tenant_role, workspace_role) in [
        (EDITOR, "member", Some("editor")),
        (VIEWER, "member", Some("viewer")),
        (OUTSIDER, "member", Some("editor")),
        (STRANGER, "member", None),
    ] {
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
        sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, $4, 'active')")
            .bind(format!("tm_{}", &user[4..]))
            .bind(TENANT)
            .bind(user)
            .bind(tenant_role)
            .execute(&mut *tx)
            .await
            .expect("tenant membership");
        if let Some(role) = workspace_role {
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
    }
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0C0EAB', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([{ "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    let state = ApiState::new(pool.clone()).expect("api");
    Some(Harness {
        router: quansio_server::api::router(state),
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

async fn thread_events(pool: &sqlx::PgPool, thread_id: &str) -> Vec<RuntimeEvent> {
    EventStore::new(pool.clone())
        .events_for_aggregate(TENANT, "thread", thread_id)
        .await
        .expect("events")
}

fn types(events: &[RuntimeEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| event.event_type.to_string())
        .collect()
}

async fn group_thread(h: &Harness, id: u32, members: &[&str]) -> String {
    let participants: Vec<Value> = members
        .iter()
        .map(|user| json!({ "kind": "user", "id": user }))
        .collect();
    let (status, created) = command(
        &h.router,
        OWNER,
        "CreateThread",
        id,
        json!({ "workspace_id": WORKSPACE, "kind": "group", "title": "launch", "participants": participants }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    created["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string()
}

#[tokio::test]
async fn a_thread_list_decides_access_exactly_as_one_thread_does() {
    let Some(h) = prepare("collab_access_many").await else {
        common::blocked_marker();
        return;
    };
    // A protected group thread, a second with other members, and an open objective thread.
    let group = group_thread(&h, 1, &[EDITOR]).await;
    let other = group_thread(&h, 2, &[VIEWER]).await;
    let (status, open) = command(
        &h.router,
        OWNER,
        "CreateThread",
        3,
        json!({ "workspace_id": WORKSPACE, "kind": "objective", "title": "plan" }),
    )
    .await;
    assert_eq!(status, 200, "{open}");
    let open = open["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let missing = "thr_01J8Z3K6F1N8VQ2X5W9Y0C0EZZ".to_string();
    let ids = vec![group, other, open, missing];
    for reader in [
        Some(OWNER),
        Some(EDITOR),
        Some(VIEWER),
        Some(OUTSIDER),
        None,
    ] {
        let many = thread_access_many(&h.pool, TENANT, &ids, reader)
            .await
            .expect("access for the list");
        for id in &ids {
            let one = thread_access(&h.pool, TENANT, id, reader)
                .await
                .expect("access");
            let listed = many
                .get(id)
                .cloned()
                .unwrap_or(quansio_server::control::collaboration::Access::Missing);
            assert_eq!(listed, one, "{reader:?} on {id}");
        }
    }
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn membership_rules_protect_a_thread_for_its_participants() {
    let Some(h) = prepare("collab_rules").await else {
        common::blocked_marker();
        return;
    };
    let thread = group_thread(&h, 1, &[EDITOR]).await;
    let (status, view) = get(&h.router, OWNER, &format!("/v1/threads/{thread}")).await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["protected"], true);
    assert_eq!(view["my_role"], "owner");
    let who: Vec<(String, String)> = view["participants"]
        .as_array()
        .expect("participants")
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap_or("").to_string(),
                p["role"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        who,
        vec![
            (OWNER.to_string(), "owner".to_string()),
            (EDITOR.to_string(), "member".to_string())
        ]
    );

    // A workspace editor who is not a participant sees nothing of it.
    for path in [
        format!("/v1/messages?thread_id={thread}"),
        format!("/v1/threads/{thread}"),
        format!("/v1/questions?thread_id={thread}"),
    ] {
        let (status, refused) = get(&h.router, OUTSIDER, &path).await;
        assert_eq!(status, 403, "{path}: {refused}");
        assert_eq!(refused["code"], "SCOPE_FORBIDDEN");
        assert_eq!(refused["details"]["kind"], "thread");
    }
    let (_, listed) = get(
        &h.router,
        OUTSIDER,
        &format!("/v1/threads?workspace_id={WORKSPACE}"),
    )
    .await;
    assert!(
        !listed["threads"]
            .as_array()
            .expect("threads")
            .iter()
            .any(|t| t["id"] == thread.as_str()),
        "{listed}"
    );
    let (_, listed) = get(
        &h.router,
        EDITOR,
        &format!("/v1/threads?workspace_id={WORKSPACE}"),
    )
    .await;
    assert!(
        listed["threads"]
            .as_array()
            .expect("threads")
            .iter()
            .any(|t| t["id"] == thread.as_str() && t["protected"] == true),
        "{listed}"
    );
    let (status, refused) = command(
        &h.router,
        OUTSIDER,
        "PostMessage",
        2,
        json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "let me in" }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    // A viewer may not author at all.
    let (status, refused) = command(
        &h.router,
        VIEWER,
        "PostMessage",
        3,
        json!({ "workspace_id": WORKSPACE, "content": "hi" }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["details"]["kind"], "rbac");
    let (status, _) = get(
        &h.router,
        EDITOR,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    assert_eq!(status, 200);

    // A member adds a member but may not grant ownership or remove anyone else.
    let (status, added) = command(
        &h.router,
        EDITOR,
        "AddParticipant",
        4,
        json!({ "thread_id": thread, "kind": "user", "id": OUTSIDER }),
    )
    .await;
    assert_eq!(status, 200, "{added}");
    assert_eq!(added["result"]["changed"], true);
    let (status, again) = command(
        &h.router,
        EDITOR,
        "AddParticipant",
        5,
        json!({ "thread_id": thread, "kind": "user", "id": OUTSIDER }),
    )
    .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        again["result"]["changed"], false,
        "adding a participant twice changes nothing"
    );
    let (status, refused) = command(
        &h.router,
        EDITOR,
        "AddParticipant",
        6,
        json!({ "thread_id": thread, "kind": "user", "id": VIEWER, "role": "owner" }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["details"]["reason"], "not_an_owner");
    let (status, refused) = command(
        &h.router,
        EDITOR,
        "RemoveParticipant",
        7,
        json!({ "thread_id": thread, "kind": "user", "id": OUTSIDER }),
    )
    .await;
    assert_eq!(status, 403, "{refused}");
    // Someone without a workspace membership cannot join.
    let (status, refused) = command(
        &h.router,
        OWNER,
        "AddParticipant",
        8,
        json!({ "thread_id": thread, "kind": "user", "id": STRANGER }),
    )
    .await;
    assert_eq!(status, 404, "{refused}");
    let (_, now) = get(
        &h.router,
        OUTSIDER,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    assert!(
        now["messages"].is_array(),
        "the added participant reads the thread: {now}"
    );

    // The last owner cannot leave people behind; after granting ownership they can.
    let (status, refused) = command(
        &h.router,
        OWNER,
        "RemoveParticipant",
        9,
        json!({ "thread_id": thread, "kind": "user", "id": OWNER }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["code"], "CONFLICT_STATE");
    let (status, _) = command(
        &h.router,
        OWNER,
        "AddParticipant",
        10,
        json!({ "thread_id": thread, "kind": "user", "id": EDITOR, "role": "owner" }),
    )
    .await;
    assert_eq!(status, 200);
    let (status, left) = command(
        &h.router,
        OWNER,
        "RemoveParticipant",
        11,
        json!({ "thread_id": thread, "kind": "user", "id": OWNER }),
    )
    .await;
    assert_eq!(status, 200, "{left}");
    assert_eq!(left["result"]["participant"]["active"], false);
    // A workspace admin who is not a participant does not read the thread — and still
    // manages its membership.
    let (status, _) = get(
        &h.router,
        OWNER,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    assert_eq!(status, 403);
    let (status, removed) = command(
        &h.router,
        OWNER,
        "RemoveParticipant",
        12,
        json!({ "thread_id": thread, "kind": "user", "id": OUTSIDER }),
    )
    .await;
    assert_eq!(status, 200, "{removed}");

    // An objective thread is the workspace's.
    let (status, objective) = command(
        &h.router,
        EDITOR,
        "CreateThread",
        13,
        json!({ "workspace_id": WORKSPACE, "kind": "objective", "title": "shared" }),
    )
    .await;
    assert_eq!(status, 200, "{objective}");
    let objective = objective["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let (status, _) = get(
        &h.router,
        OUTSIDER,
        &format!("/v1/messages?thread_id={objective}"),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = get(
        &h.router,
        STRANGER,
        &format!("/v1/messages?thread_id={objective}"),
    )
    .await;
    assert_eq!(
        status, 403,
        "a tenant member without the workspace reads no workspace thread"
    );

    let events = types(&thread_events(&h.pool, &thread).await);
    assert_eq!(events[0], "thread.created");
    assert_eq!(
        events
            .iter()
            .filter(|t| *t == "thread.participant_added")
            .count(),
        4,
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|t| *t == "thread.participant_removed")
            .count(),
        2,
        "{events:?}"
    );
    let versions: Vec<u64> = thread_events(&h.pool, &thread)
        .await
        .iter()
        .map(|e| e.aggregate_version)
        .collect();
    assert_eq!(
        versions,
        (1..=versions.len() as u64).collect::<Vec<_>>(),
        "thread events carry the thread's own versions"
    );
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_removed_participant_receives_no_new_protected_event() {
    let Some(h) = prepare("collab_revoke").await else {
        common::blocked_marker();
        return;
    };
    let thread = group_thread(&h, 1, &[EDITOR]).await;
    let (status, first) = command(
        &h.router,
        OWNER,
        "PostMessage",
        2,
        json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "before" }),
    )
    .await;
    assert_eq!(status, 200, "{first}");
    let store = EventStore::new(h.pool.clone());
    let mut editor = ThreadGate::new(h.pool.clone(), TENANT, Some(EDITOR));
    let mut owner = ThreadGate::new(h.pool.clone(), TENANT, Some(OWNER));
    let mut outsider = ThreadGate::new(h.pool.clone(), TENANT, Some(OUTSIDER));
    let before = store
        .read_events_after(TENANT, None, 10_000)
        .await
        .expect("events");
    let mut seen = Vec::new();
    for event in &before {
        let for_editor = editor.admits_event(event).await;
        assert_eq!(
            for_editor,
            owner.admits_event(event).await,
            "{}",
            event.event_type
        );
        if event.aggregate_id == thread {
            assert!(for_editor, "a participant receives {}", event.event_type);
            assert!(
                !outsider.admits_event(event).await,
                "a non-participant never receives {}",
                event.event_type
            );
            seen.push(event.event_type.to_string());
        }
    }
    assert!(
        seen.contains(&"thread.message_posted".to_string()),
        "{seen:?}"
    );
    let last = before.last().map(|event| event.sequence);

    // The owner removes the editor, then keeps talking.
    let (status, _) = command(
        &h.router,
        OWNER,
        "RemoveParticipant",
        3,
        json!({ "thread_id": thread, "kind": "user", "id": EDITOR }),
    )
    .await;
    assert_eq!(status, 200);
    let (status, second) = command(
        &h.router,
        OWNER,
        "PostMessage",
        4,
        json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "after" }),
    )
    .await;
    assert_eq!(status, 200, "{second}");
    let after: Vec<RuntimeEvent> = store
        .read_events_after(TENANT, last, 10_000)
        .await
        .expect("events");
    let mut editor_received = Vec::new();
    for event in &after {
        if editor.admits_event(event).await && event.aggregate_id == thread {
            editor_received.push(event.event_type.to_string());
        }
        if event.aggregate_id == thread {
            assert!(
                owner.admits_event(event).await,
                "the owner keeps receiving {}",
                event.event_type
            );
        }
    }
    assert_eq!(
        editor_received,
        vec!["thread.participant_removed".to_string()],
        "the removed person hears their removal and nothing after it"
    );

    // A live frame of the new run is the thread's content in flight: withheld too.
    let run_id = second["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let frame = LiveFrame {
        channel: format!("workspace:{WORKSPACE}"),
        frame: json!({ "kind": "model.delta", "text": "secret", "run_id": run_id }),
    };
    assert!(!editor.admits_live(&frame).await);
    assert!(owner.admits_live(&frame).await);
    assert!(
        !outsider
            .admits_live(&LiveFrame {
                channel: format!("thread:{thread}"),
                frame: json!({ "kind": "progress" })
            })
            .await
    );

    // Reads and writes are refused as well.
    let (status, _) = get(
        &h.router,
        EDITOR,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = command(
        &h.router,
        EDITOR,
        "PostMessage",
        5,
        json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "hello?" }),
    )
    .await;
    assert_eq!(status, 403);
    let message_id = second["result"]["message_id"].as_str().expect("message");
    let (status, _) = command(
        &h.router,
        EDITOR,
        "ReactToMessage",
        6,
        json!({ "message_id": message_id, "emoji": "👀" }),
    )
    .await;
    assert_eq!(status, 403);

    // Re-added, the next event reaches them again.
    let mark = store
        .read_events_after(TENANT, None, 10_000)
        .await
        .expect("events")
        .last()
        .map(|e| e.sequence);
    let (status, _) = command(
        &h.router,
        OWNER,
        "AddParticipant",
        7,
        json!({ "thread_id": thread, "kind": "user", "id": EDITOR }),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = command(
        &h.router,
        OWNER,
        "PostMessage",
        8,
        json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "welcome back" }),
    )
    .await;
    assert_eq!(status, 200);
    let mut back = Vec::new();
    for event in store
        .read_events_after(TENANT, mark, 10_000)
        .await
        .expect("events")
    {
        if event.aggregate_id == thread && editor.admits_event(&event).await {
            back.push(event.event_type.to_string());
        }
    }
    assert_eq!(
        back.first().map(String::as_str),
        Some("thread.participant_added")
    );
    assert!(
        back.contains(&"thread.message_posted".to_string()),
        "{back:?}"
    );
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn concurrent_posts_from_two_people_keep_one_stable_order() {
    let Some(h) = prepare("collab_order").await else {
        common::blocked_marker();
        return;
    };
    let thread = group_thread(&h, 1, &[EDITOR]).await;
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..10u32 {
        let router = h.router.clone();
        let thread = thread.clone();
        let user = if n % 2 == 0 { OWNER } else { EDITOR };
        tasks.spawn(async move {
            command(&router, user, "PostMessage", 100 + n, json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": format!("message {n}") })).await
        });
    }
    let mut posted = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (status, body) = result.expect("task");
        assert_eq!(status, 200, "{body}");
        posted.push(
            body["result"]["message_id"]
                .as_str()
                .expect("id")
                .to_string(),
        );
    }
    let (_, listed) = get(
        &h.router,
        OWNER,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    let messages = listed["messages"].as_array().expect("messages").clone();
    let seqs: Vec<i64> = messages
        .iter()
        .map(|m| m["seq"].as_i64().expect("seq"))
        .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as i64).collect::<Vec<_>>(),
        "gap-free, distinct seq"
    );
    let ids: std::collections::HashSet<&str> =
        messages.iter().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(ids.len(), messages.len());
    for id in &posted {
        assert!(
            ids.contains(id.as_str()),
            "every accepted post is in the thread"
        );
    }
    // The order is the same for every reader and every read.
    let (_, again) = get(
        &h.router,
        EDITOR,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    assert_eq!(again["messages"], listed["messages"]);
    // The durable events commit in seq order: the stream shows every client one order.
    let posted_events: Vec<i64> = thread_events(&h.pool, &thread)
        .await
        .iter()
        .filter(|event| event.event_type.to_string() == "thread.message_posted")
        .map(|event| event.payload["seq"].as_i64().expect("seq"))
        .collect();
    assert_eq!(posted_events, seqs);
    let versions: Vec<u64> = thread_events(&h.pool, &thread)
        .await
        .iter()
        .map(|e| e.aggregate_version)
        .collect();
    assert_eq!(versions, (1..=versions.len() as u64).collect::<Vec<_>>());
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn reactions_mentions_and_attention() {
    let Some(h) = prepare("collab_react").await else {
        common::blocked_marker();
        return;
    };
    let thread = group_thread(&h, 1, &[EDITOR]).await;
    let (status, refused) = command(&h.router, OWNER, "PostMessage", 2, json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "@outsider?", "mentions": [{ "kind": "user", "id": OUTSIDER }] })).await;
    assert_eq!(
        status, 409,
        "a mention of someone outside a protected thread: {refused}"
    );
    let (status, posted) = command(&h.router, OWNER, "PostMessage", 3, json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "@editor can you look?", "mentions": [{ "kind": "user", "id": EDITOR }] })).await;
    assert_eq!(status, 200, "{posted}");
    let message_id = posted["result"]["message_id"]
        .as_str()
        .expect("message")
        .to_string();
    let (_, view) = get(&h.router, EDITOR, &format!("/v1/threads/{thread}")).await;
    let editor_attention = view["attention"]
        .as_array()
        .expect("attention")
        .iter()
        .find(|a| a["id"] == EDITOR)
        .cloned()
        .expect("editor");
    assert_eq!(editor_attention["mentions_pending"], 1, "{view}");
    assert_eq!(editor_attention["needs_human"], true);
    let (_, listed) = get(
        &h.router,
        EDITOR,
        &format!("/v1/threads?workspace_id={WORKSPACE}"),
    )
    .await;
    let entry = listed["threads"]
        .as_array()
        .expect("threads")
        .iter()
        .find(|t| t["id"] == thread.as_str())
        .cloned()
        .expect("thread");
    assert_eq!(entry["attention"]["needs_you"], true, "{entry}");
    let (_, messages) = get(
        &h.router,
        EDITOR,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    let mentioned = messages["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["id"] == message_id.as_str())
        .cloned()
        .expect("message");
    assert_eq!(
        mentioned["mentions"],
        json!([{ "kind": "user", "id": EDITOR }])
    );
    // Answering clears it.
    let (status, _) = command(
        &h.router,
        EDITOR,
        "PostMessage",
        4,
        json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "on it" }),
    )
    .await;
    assert_eq!(status, 200);
    let (_, view) = get(&h.router, EDITOR, &format!("/v1/threads/{thread}")).await;
    let editor_attention = view["attention"]
        .as_array()
        .expect("attention")
        .iter()
        .find(|a| a["id"] == EDITOR)
        .cloned()
        .expect("editor");
    assert_eq!(editor_attention["mentions_pending"], 0, "{view}");
    let attention_events: Vec<String> = thread_events(&h.pool, &thread)
        .await
        .iter()
        .filter(|event| event.event_type.to_string() == "thread.attention_changed")
        .map(|event| {
            event.payload["attention"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(attention_events, vec!["mentioned", "none"]);

    // Reactions: idempotent per person and emoji.
    let react = |n: u32, user: &'static str, emoji: &'static str, remove: bool| {
        let router = h.router.clone();
        let message_id = message_id.clone();
        async move {
            command(
                &router,
                user,
                "ReactToMessage",
                n,
                json!({ "message_id": message_id, "emoji": emoji, "remove": remove }),
            )
            .await
        }
    };
    let (status, first) = react(10, EDITOR, "👍", false).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["result"]["changed"], true);
    let (_, twice) = react(11, EDITOR, "👍", false).await;
    assert_eq!(twice["result"]["changed"], false);
    let (_, owner) = react(12, OWNER, "👍", false).await;
    assert_eq!(
        owner["result"]["reactions"].as_array().map(Vec::len),
        Some(2)
    );
    let (_, removed) = react(13, EDITOR, "👍", true).await;
    assert_eq!(
        removed["result"]["reactions"],
        json!([{ "emoji": "👍", "user_id": OWNER }])
    );
    let (status, bad) = react(14, EDITOR, "two words", false).await;
    assert_eq!(status, 400, "{bad}");
    let (status, _) = react(15, OUTSIDER, "👍", false).await;
    assert_eq!(status, 403);
    let reaction_events: Vec<String> = types(&thread_events(&h.pool, &thread).await)
        .into_iter()
        .filter(|t| t.starts_with("thread.reaction_"))
        .collect();
    assert_eq!(
        reaction_events,
        vec![
            "thread.reaction_added",
            "thread.reaction_added",
            "thread.reaction_removed"
        ]
    );
    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn a_handoff_moves_the_thread_to_the_next_teammate() {
    let Some(h) = prepare("collab_handoff").await else {
        common::blocked_marker();
        return;
    };
    let (status, atlas) = command(
        &h.router,
        OWNER,
        "CreateTeammate",
        1,
        json!({ "workspace_id": WORKSPACE, "template_id": "engineer", "name": "Atlas" }),
    )
    .await;
    assert_eq!(status, 200, "{atlas}");
    let atlas = atlas["result"]["id"].as_str().expect("agt").to_string();
    let (status, nova) = command(
        &h.router,
        OWNER,
        "CreateTeammate",
        2,
        json!({ "workspace_id": WORKSPACE, "template_id": "researcher", "name": "Nova" }),
    )
    .await;
    assert_eq!(status, 200, "{nova}");
    let nova_thread = nova["result"]["thread"]["id"]
        .as_str()
        .expect("ath")
        .to_string();
    let nova = nova["result"]["id"].as_str().expect("agt").to_string();

    // A conversation with Atlas: the teammate joins the thread it is addressed in.
    let (status, first) = command(
        &h.router,
        OWNER,
        "PostMessage",
        3,
        json!({ "workspace_id": WORKSPACE, "content": "draft the plan", "teammate_id": atlas }),
    )
    .await;
    assert_eq!(status, 200, "{first}");
    let thread = first["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let (_, view) = get(&h.router, OWNER, &format!("/v1/threads/{thread}")).await;
    assert_eq!(view["responsible_teammate_id"], atlas.as_str(), "{view}");
    assert_eq!(view["kind"], "direct");

    // A viewer may not hand off; handing to the current teammate is refused.
    let (status, _) = command(
        &h.router,
        VIEWER,
        "HandOffThread",
        4,
        json!({ "thread_id": thread, "to_teammate_id": nova }),
    )
    .await;
    assert_eq!(status, 403);
    let (status, same) = command(
        &h.router,
        OWNER,
        "HandOffThread",
        5,
        json!({ "thread_id": thread, "to_teammate_id": atlas }),
    )
    .await;
    assert_eq!(status, 409, "{same}");

    let (status, handed) = command(
        &h.router,
        OWNER,
        "HandOffThread",
        6,
        json!({ "thread_id": thread, "to_teammate_id": nova }),
    )
    .await;
    assert_eq!(status, 200, "{handed}");
    assert_eq!(handed["result"]["from"]["id"], atlas.as_str());
    assert_eq!(handed["result"]["from"]["active"], false);
    assert_eq!(handed["result"]["to"]["id"], nova.as_str());
    assert_eq!(
        handed["result"]["mailbox"]["agent_thread_id"],
        nova_thread.as_str()
    );
    let notice_id = handed["result"]["notice_message_id"]
        .as_str()
        .expect("notice")
        .to_string();
    let (_, messages) = get(
        &h.router,
        OWNER,
        &format!("/v1/messages?thread_id={thread}"),
    )
    .await;
    let notice = messages["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["id"] == notice_id.as_str())
        .cloned()
        .expect("notice");
    assert_eq!(notice["author_kind"], "system");
    assert_eq!(notice["content_blocks"][0]["type"], "system_notice");
    assert_eq!(
        notice["content_blocks"][0]["notice"]["to"]["id"],
        nova.as_str()
    );
    assert_eq!(
        notice["content_blocks"][0]["text"],
        "Atlas handed this thread to Nova."
    );
    let events = types(&thread_events(&h.pool, &thread).await);
    assert!(
        events.contains(&"thread.handed_off".to_string()),
        "{events:?}"
    );
    // RUN-002 carried it into Nova's mailbox, with Atlas's last run.
    let mut conn = h.pool.acquire().await.expect("conn");
    schema::set_tenant_context_conn(&mut conn, TENANT)
        .await
        .expect("tenant");
    let payload: Value = sqlx::query_scalar("SELECT payload FROM agent_mailbox_items WHERE tenant_id = $1 AND agent_thread_id = $2 AND message_id = $3")
        .bind(TENANT)
        .bind(&nova_thread)
        .bind(format!("thread-handoff:{notice_id}"))
        .fetch_one(&mut *conn)
        .await
        .expect("mailbox item");
    assert_eq!(payload["kind"], "thread_handoff");
    assert_eq!(payload["last_run_id"], first["result"]["run_id"]);

    // The next un-addressed message goes to Nova.
    let (status, next) = command(
        &h.router,
        OWNER,
        "PostMessage",
        7,
        json!({ "workspace_id": WORKSPACE, "thread_id": thread, "content": "status?" }),
    )
    .await;
    assert_eq!(status, 200, "{next}");
    assert_eq!(next["result"]["teammate"]["id"], nova.as_str());
    let (_, view) = get(&h.router, OWNER, &format!("/v1/threads/{thread}")).await;
    assert_eq!(view["responsible_teammate_id"], nova.as_str());

    // While Nova still has a live run in the thread, it cannot hand the thread on.
    let trigger = next["result"]["message_id"]
        .as_str()
        .expect("message")
        .to_string();
    sqlx::query(
        "UPDATE runs SET status = 'WAITING_QUESTION' WHERE tenant_id = $1 AND trigger_ref = $2",
    )
    .bind(TENANT)
    .bind(&trigger)
    .execute(&mut *conn)
    .await
    .expect("park run");
    let (status, live) = command(
        &h.router,
        OWNER,
        "HandOffThread",
        8,
        json!({ "thread_id": thread, "to_teammate_id": atlas }),
    )
    .await;
    assert_eq!(status, 409, "{live}");
    assert!(
        live["message"]
            .as_str()
            .unwrap_or("")
            .contains("WAITING_QUESTION"),
        "{live}"
    );
    drop(conn);
    common::drop_pool(&h.pool, &h.name).await;
}
