//! APP-005: the plan/WorkGraph surface — the projection matches the canonical revision,
//! user edits are validated graph commands (a stale revision is `CONFLICT_REVISION`, a
//! cycle is refused, nothing is written on refusal), objectives are created through the
//! same path, and an agent handoff shows up as a delegation edge with the run parked.

use axum::http::Request;
use quansio_server::api::seams::DELEGATE_SCENARIO;
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0MMMMM";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0M{n:04}")
}

async fn prepare(prefix: &str) -> Option<(axum::Router, sqlx::PgPool, String)> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }
    ]);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0MMMMM', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("policy");
    tx.commit().await.expect("commit");
    let state = ApiState::new(pool.clone()).expect("api");
    Some((quansio_server::api::router(state), pool, name))
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

async fn graph(router: &axum::Router) -> Value {
    let response = router
        .clone()
        .oneshot(http(
            "GET",
            &format!("/v1/workgraph?workspace_id={WORKSPACE}"),
            None,
        ))
        .await
        .expect("response");
    assert_eq!(response.status().as_u16(), 200);
    json_body(response).await
}

fn contract() -> Value {
    json!({
        "deterministic_checks": [{ "kind": "artifact_exists", "artifact_role": "report", "min_versions": 1 }],
        "semantic_verification": { "required": false },
        "human_signoff_required": false
    })
}

fn add_task(title: &str, parent: &str, extra: Value) -> Value {
    let mut node = json!({
        "id": title,
        "kind": "task",
        "title": title,
        "parent_id": parent,
        "status": "draft",
        "completion_contract": contract(),
        "capability_needs": [],
        "priority": 1
    });
    if let Some(object) = extra.as_object() {
        for (key, value) in object {
            node[key] = value.clone();
        }
    }
    node
}

#[tokio::test]
async fn the_projection_is_the_canonical_revision_and_edits_are_validated_commands() {
    let Some((router, pool, name)) = prepare("wg_edit").await else {
        common::blocked_marker();
        return;
    };
    let before = graph(&router).await;
    let revision = before["revision"].as_u64().expect("revision");
    assert_eq!(
        before["nodes"].as_array().map(Vec::len),
        Some(1),
        "{before}"
    );
    assert_eq!(before["nodes"][0]["id"], NODE, "{before}");

    // An objective through CreateObjective: a node bound to its objective thread.
    let (status, created) = command(
        &router,
        "CreateObjective",
        1,
        json!({ "workspace_id": WORKSPACE, "title": "Ship the report", "description": "quarterly" }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    let objective_id = created["result"]["objective_id"]
        .as_str()
        .expect("objective")
        .to_string();
    assert!(objective_id.starts_with("wn_"), "{created}");
    let thread_id = created["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    assert_eq!(
        created["result"]["revision"].as_u64(),
        Some(revision + 1),
        "{created}"
    );
    let with_objective = graph(&router).await;
    let objective = with_objective["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == objective_id)
        .cloned()
        .expect("objective in the projection");
    assert_eq!(objective["kind"], "objective", "{objective}");
    assert_eq!(objective["thread_id"], thread_id, "{objective}");
    assert_eq!(objective["created_by"]["id"], USER, "{objective}");
    assert_eq!(
        objective["completion_contract"]["human_signoff_required"], true,
        "{objective}"
    );
    let revision = with_objective["revision"].as_u64().expect("revision");

    // A plan edit against the observed revision: two tasks under the objective. The
    // projection after equals the revision it reports. (An edge may only reference nodes
    // that already exist — the planner's rule — so the dependency is the next edit.)
    let (status, applied) = command(
        &router,
        "ProposeGraphEdit",
        2,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": revision,
            "nodes_add": [add_task("gather data", &objective_id, json!({})), add_task("write report", &objective_id, json!({}))],
            "rationale": "the user laid out the plan"
        }),
    )
    .await;
    assert_eq!(status, 200, "{applied}");
    assert_eq!(applied["result"]["nodes_added"], 2, "{applied}");
    let added = graph(&router).await;
    assert_eq!(
        added["revision"].as_u64(),
        applied["result"]["revision"].as_u64(),
        "{added}"
    );
    let titles: Vec<&str> = added["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["title"].as_str())
        .collect();
    assert!(
        titles.contains(&"gather data") && titles.contains(&"write report"),
        "{added}"
    );
    let write = added["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["title"] == "write report")
        .cloned()
        .unwrap();
    let gather = added["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["title"] == "gather data")
        .cloned()
        .unwrap();
    assert_eq!(write["parent_id"], objective_id, "{added}");
    let (status, linked) = command(
        &router,
        "ProposeGraphEdit",
        7,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": added["revision"],
            "edges_add": [{ "id": "dep", "from_node_id": write["id"], "to_node_id": gather["id"], "kind": "depends_on" }],
            "rationale": "write depends on gather"
        }),
    )
    .await;
    assert_eq!(status, 200, "{linked}");
    assert_eq!(linked["result"]["edges_added"], 1, "{linked}");
    let after = graph(&router).await;
    assert_eq!(
        after["revision"].as_u64(),
        linked["result"]["revision"].as_u64(),
        "{after}"
    );
    let edge = after["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "depends_on")
        .expect("edge");
    assert_eq!(edge["from_node_id"], write["id"], "{after}");
    assert_eq!(edge["to_node_id"], gather["id"], "{after}");
    // The dependent is a blocker until its prerequisite is done.
    let blocker = after["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["node_id"] == write["id"])
        .expect("blocker");
    assert_eq!(blocker["kind"], "prerequisite", "{after}");
    assert_eq!(blocker["ref"], gather["id"], "{after}");

    // The same base revision again: a concurrent editor's stale view is a conflict, and
    // nothing is written.
    let (status, conflict) = command(
        &router,
        "ProposeGraphEdit",
        3,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": revision,
            "nodes_add": [add_task("late task", &objective_id, json!({}))],
            "rationale": "edited an old view"
        }),
    )
    .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(conflict["code"], "CONFLICT_REVISION", "{conflict}");
    let unchanged = graph(&router).await;
    assert_eq!(unchanged["revision"], after["revision"], "{unchanged}");
    assert_eq!(
        unchanged["nodes"].as_array().map(Vec::len),
        after["nodes"].as_array().map(Vec::len),
        "{unchanged}"
    );

    // Rebased onto the current revision, the same intent applies.
    let (status, rebased) = command(
        &router,
        "ProposeGraphEdit",
        4,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": after["revision"],
            "nodes_add": [add_task("late task", &objective_id, json!({}))],
            "rationale": "rebased"
        }),
    )
    .await;
    assert_eq!(status, 200, "{rebased}");

    // An edge naming a node the same edit creates is refused (edges need canonical ids).
    let (status, local) = command(
        &router,
        "ProposeGraphEdit",
        8,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": graph(&router).await["revision"],
            "nodes_add": [add_task("new one", &objective_id, json!({}))],
            "edges_add": [{ "id": "e", "from_node_id": "new one", "to_node_id": gather["id"], "kind": "depends_on" }],
            "rationale": "edge to a node not yet created"
        }),
    )
    .await;
    assert_eq!(status, 400, "{local}");
    assert_eq!(local["code"], "VALIDATION_SCHEMA", "{local}");

    // A cycle is refused with nothing written; a widening capability need is refused.
    let current = graph(&router).await;
    let (status, cycle) = command(
        &router,
        "ProposeGraphEdit",
        5,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": current["revision"],
            "edges_add": [{ "id": "back", "from_node_id": gather["id"], "to_node_id": write["id"], "kind": "depends_on" }],
            "rationale": "a loop"
        }),
    )
    .await;
    assert_ne!(status, 200, "{cycle}");
    assert_eq!(graph(&router).await["revision"], current["revision"]);
    let (status, widening) = command(
        &router,
        "ProposeGraphEdit",
        6,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": current["revision"],
            "nodes_add": [add_task("send mail", &objective_id, json!({
                "capability_needs": [{ "effect_class": "message.send", "resource": { "kind": "domain", "selector": "*" }, "constraints": {} }]
            }))],
            "capability_needs": [{ "effect_class": "message.send", "resource": { "kind": "domain", "selector": "*" }, "constraints": {} }],
            "rationale": "needs more than the policy allows"
        }),
    )
    .await;
    assert_eq!(status, 403, "{widening}");
    assert_eq!(widening["code"], "CAPABILITY_DENIED", "{widening}");

    // Completion is never set through a plan: the store's own verdict reaches the client
    // with its code, and nothing is written.
    let current = graph(&router).await;
    let gather_now = current["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == gather["id"])
        .cloned()
        .unwrap();
    let (status, done) = command(
        &router,
        "ProposeGraphEdit",
        9,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": current["revision"],
            "nodes_update": [{ "node_id": gather["id"], "expected_revision": gather_now["revision"], "to": "done" }],
            "rationale": "declare it finished"
        }),
    )
    .await;
    assert_eq!(status, 409, "{done}");
    assert_eq!(done["code"], "CONFLICT_STATE", "{done}");
    assert_eq!(graph(&router).await["revision"], current["revision"]);

    // parent_of runs parent -> child like parent_id: restating the objective as gather's
    // parent applies, while gather depending on its own parent is a cycle.
    let (status, restated) = command(
        &router,
        "ProposeGraphEdit",
        10,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": current["revision"],
            "edges_add": [{ "id": "par", "from_node_id": objective_id, "to_node_id": gather["id"], "kind": "parent_of" }],
            "rationale": "restate the parent"
        }),
    )
    .await;
    assert_eq!(status, 200, "{restated}");
    let current = graph(&router).await;
    let (status, upward) = command(
        &router,
        "ProposeGraphEdit",
        11,
        json!({
            "workspace_id": WORKSPACE,
            "base_revision": current["revision"],
            "edges_add": [{ "id": "up", "from_node_id": gather["id"], "to_node_id": objective_id, "kind": "depends_on" }],
            "rationale": "a child waiting on its parent"
        }),
    )
    .await;
    assert_eq!(status, 400, "{upward}");
    assert_eq!(upward["code"], "VALIDATION_SCHEMA", "{upward}");
    assert!(upward["message"].to_string().contains("cycle"), "{upward}");

    // The plan events are on the durable log for every client.
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT type FROM runtime_events WHERE tenant_id = $1 AND type LIKE 'work.%' ORDER BY sequence",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    assert!(
        events.iter().any(|e| e == "work.plan_applied"),
        "{events:?}"
    );
    // The cycle, the widening need, the completion and the upward dependency: one
    // rejection each (the stale and local-edge refusals are recorded too).
    assert!(
        events.iter().filter(|e| *e == "work.plan_rejected").count() >= 4,
        "{events:?}"
    );

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_delegation_is_a_handoff_edge_and_a_parked_run_in_the_view() {
    let Some((router, pool, name)) = prepare("wg_handoff").await else {
        common::blocked_marker();
        return;
    };
    let (status, posted) = command(
        &router,
        "PostMessage",
        1,
        json!({ "workspace_id": WORKSPACE, "content": format!("split this up {DELEGATE_SCENARIO}") }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    assert_eq!(posted["result"]["turn"]["kind"], "parked", "{posted}");
    assert_eq!(
        posted["result"]["turn"]["state"], "WAITING_CHILD",
        "{posted}"
    );
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();

    let view = graph(&router).await;
    let agents = view["agents"].as_array().expect("agents");
    let teammate = agents
        .iter()
        .find(|a| a["kind"] == "teammate")
        .expect("teammate");
    let worker = agents
        .iter()
        .find(|a| a["kind"] == "worker")
        .expect("worker spawned by the delegation");
    assert_eq!(worker["parent_id"], teammate["id"], "{view}");
    assert_eq!(worker["work_node_id"], NODE, "{view}");
    assert_eq!(worker["status"], "ACTIVE", "{view}");
    let handoff = view["handoffs"]
        .as_array()
        .expect("handoffs")
        .first()
        .cloned()
        .expect("a handoff edge");
    assert_eq!(handoff["parent_agent_thread_id"], teammate["id"], "{view}");
    assert_eq!(handoff["child_agent_thread_id"], worker["id"], "{view}");
    assert_eq!(handoff["work_node_id"], NODE, "{view}");
    assert!(handoff["joined_at"].is_null(), "{view}");
    let node = view["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == NODE)
        .expect("node");
    assert_eq!(node["run"]["id"], run_id, "{view}");
    assert_eq!(node["run"]["status"], "WAITING_CHILD", "{view}");
    let blocker = view["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["node_id"] == NODE)
        .expect("blocker");
    assert_eq!(blocker["kind"], "run", "{view}");
    assert_eq!(blocker["status"], "WAITING_CHILD", "{view}");

    common::drop_pool(&pool, &name).await;
}
