//! APP-012: the administration and review surface is decided by the server — a viewer is
//! refused every admin projection and command with `SCOPE_FORBIDDEN`, an approver may
//! decide an approval but not administer, an owner may do all of it; memberships,
//! policies and trust decisions commit with their AuditEntry in a per-tenant hash chain
//! that a tampered row breaks; usage and connectors are served to the roles DOMAIN.md §2
//! names; and the web app is served under `/app/` with a locked policy and a path that
//! cannot leave its root.
//!
//! Runs against a scratch database (blocked, not skipped, without one).

use axum::http::Request;
use quansio_server::api::seams::APPROVAL_SCENARIO;
use quansio_server::api::web::{WebRoot, WEB_CSP};
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0WWWWW";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0WWWW1";
const VIEWER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0WWWW2";
const APPROVER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0WWWW3";
const BILLING: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0WWWW4";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0WWWWW";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0WWWWW";
const SIGNING_KEY: &str = "web-admin-surface-test-signing-key";

fn command_id(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0W{n:04}")
}

struct Harness {
    router: axum::Router,
    pool: sqlx::PgPool,
    name: String,
    _web: tempfile::TempDir,
}

async fn member(pool: &sqlx::PgPool, user: &str, tenant_role: &str, workspace_role: &str) {
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, $3)")
        .bind(user)
        .bind(format!("{user}@example.com"))
        .bind(format!("{tenant_role}/{workspace_role}"))
        .execute(&mut *tx)
        .await
        .expect("user");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let suffix = user.trim_start_matches("usr_");
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, $4, 'active')")
        .bind(format!("tm_{suffix}"))
        .bind(TENANT)
        .bind(user)
        .bind(tenant_role)
        .execute(&mut *tx)
        .await
        .expect("tenant membership");
    sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ($1, $2, $3, $4, $5, 'active')")
        .bind(format!("wm_{suffix}"))
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(user)
        .bind(workspace_role)
        .execute(&mut *tx)
        .await
        .expect("workspace membership");
    tx.commit().await.expect("commit");
}

async fn prepare(prefix: &str) -> Option<Harness> {
    std::env::set_var("QUANSIO_APPROVAL_SIGNING_KEY", SIGNING_KEY);
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    member(&pool, VIEWER, "member", "viewer").await;
    member(&pool, APPROVER, "member", "approver").await;
    member(&pool, BILLING, "billing", "viewer").await;
    let web = tempfile::tempdir().expect("web root");
    std::fs::write(web.path().join("index.html"), "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"></head><body><div id=\"root\"></div><script src=\"/app/assets/app.js\"></script></body></html>").expect("index");
    std::fs::create_dir(web.path().join("assets")).expect("assets");
    std::fs::write(web.path().join("assets/app.js"), "console.log('web');").expect("js");
    std::fs::write(
        web.path().join("assets/app.css"),
        "@media (max-width: 720px) { nav { display: none } }",
    )
    .expect("css");
    let state = ApiState::new(pool.clone())
        .expect("api")
        .with_web_root(WebRoot::new(web.path()).expect("root"));
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
        _web: web,
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

fn forbidden(status: u16, body: &Value, action: &str) {
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["code"], "SCOPE_FORBIDDEN", "{body}");
    assert_eq!(body["details"]["kind"], "rbac", "{body}");
    assert_eq!(body["details"]["action"], action, "{body}");
}

#[tokio::test]
async fn admin_pages_and_commands_are_decided_by_the_servers_rbac_for_every_role() {
    let Some(h) = prepare("web_rbac").await else {
        common::blocked_marker();
        return;
    };
    // A viewer holds a workspace role and nothing more: every admin projection refuses.
    for (path, action) in [
        ("/v1/members", "manage_workspace"),
        (
            &format!("/v1/policies?workspace_id={WORKSPACE}"),
            "manage_workspace",
        ),
        ("/v1/connectors", "manage_workspace"),
        ("/v1/usage", "view_usage"),
        ("/v1/audit", "manage_tenant"),
    ] {
        let (status, body) = get(&h.router, VIEWER, path).await;
        forbidden(status, &body, action);
        assert_eq!(
            body["details"]["workspace_role"],
            if path.contains("workspace_id") {
                json!("viewer")
            } else {
                Value::Null
            },
            "{body}"
        );
    }
    // Admin commands refuse the viewer and the approver alike, before any owner is called.
    let (status, body) = command(
        &h.router,
        VIEWER,
        "SetPolicy",
        1,
        json!({ "scope": "tenant", "rules": [] }),
    )
    .await;
    forbidden(status, &body, "manage_tenant");
    let (status, body) = command(
        &h.router,
        APPROVER,
        "InviteMember",
        2,
        json!({ "email": "x@example.com", "role": "member" }),
    )
    .await;
    forbidden(status, &body, "manage_tenant");
    let (status, body) = command(
        &h.router,
        APPROVER,
        "SetPolicy",
        3,
        json!({ "scope": "workspace", "workspace_id": WORKSPACE, "rules": [] }),
    )
    .await;
    forbidden(status, &body, "manage_workspace");
    let (status, body) = command(
        &h.router,
        VIEWER,
        "RemoveMember",
        4,
        json!({ "membership_id": format!("tm_{}", &OWNER[4..]) }),
    )
    .await;
    forbidden(status, &body, "manage_tenant");
    // Billing sees usage only.
    let (status, usage) = get(&h.router, BILLING, "/v1/usage").await;
    assert_eq!(status, 200, "{usage}");
    assert!(
        usage["meters"]
            .as_array()
            .is_some_and(|meters| meters.len() == 8),
        "{usage}"
    );
    let (status, body) = get(&h.router, BILLING, "/v1/audit").await;
    forbidden(status, &body, "manage_tenant");
    // A user with no membership at all holds no role.
    let (status, body) = get(&h.router, "usr_01J8Z3K6F1N8VQ2X5W9Y0WWWW9", "/v1/members").await;
    forbidden(status, &body, "manage_workspace");
    assert_eq!(body["details"]["reason"], "rbac_no_role", "{body}");

    // The owner administers: members are listed with both scopes of role.
    let (status, members) = get(&h.router, OWNER, "/v1/members").await;
    assert_eq!(status, 200, "{members}");
    let listed = members["members"].as_array().expect("members");
    assert_eq!(listed.len(), 4, "{members}");
    let viewer = listed
        .iter()
        .find(|row| row["user"]["id"] == VIEWER)
        .expect("viewer");
    assert_eq!(viewer["role"], "member");
    assert_eq!(viewer["workspaces"][0]["role"], "viewer");
    let (_, of_workspace) = get(
        &h.router,
        OWNER,
        &format!("/v1/members?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(of_workspace["members"].as_array().map_or(0, Vec::len), 4);

    // Invite a new address: the user is provisioned `invited`; the membership is audited.
    let (status, invited) = command(&h.router, OWNER, "InviteMember", 5, json!({ "email": "Nova@Example.com", "role": "admin", "workspace_id": WORKSPACE, "workspace_role": "editor" })).await;
    assert_eq!(status, 200, "{invited}");
    let invited = &invited["result"];
    assert_eq!(invited["user"]["primary_email"], "nova@example.com");
    assert_eq!(invited["status"], "invited");
    assert_eq!(invited["role"], "admin");
    assert_eq!(invited["workspaces"][0]["role"], "editor");
    let membership_id = invited["membership_id"].as_str().expect("tm").to_string();
    let nova = invited["user"]["id"].as_str().expect("usr").to_string();
    // Invited is not active: Nova holds no role yet.
    let (status, body) = get(&h.router, &nova, "/v1/members").await;
    forbidden(status, &body, "manage_workspace");
    // Inviting again is a conflict; a bad address a schema error.
    let (status, again) = command(
        &h.router,
        OWNER,
        "InviteMember",
        6,
        json!({ "email": "nova@example.com", "role": "member" }),
    )
    .await;
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["code"], "CONFLICT_STATE");
    let (status, bad) = command(
        &h.router,
        OWNER,
        "InviteMember",
        7,
        json!({ "email": "not-an-address", "role": "member" }),
    )
    .await;
    assert_eq!(status, 400, "{bad}");
    // Activate and change the role: now Nova administers the tenant.
    let (status, activated) = command(
        &h.router,
        OWNER,
        "UpdateMembership",
        8,
        json!({ "membership_id": membership_id, "status": "active" }),
    )
    .await;
    assert_eq!(status, 200, "{activated}");
    assert_eq!(activated["result"]["status"], "active");
    let (status, _) = get(&h.router, &nova, "/v1/audit").await;
    assert_eq!(status, 200, "an active tenant admin reads the audit log");
    let (status, unknown_role) = command(
        &h.router,
        OWNER,
        "UpdateMembership",
        9,
        json!({ "membership_id": membership_id, "role": "emperor" }),
    )
    .await;
    assert_eq!(status, 400, "{unknown_role}");
    // The last owner cannot be demoted or removed.
    let owner_membership = format!("tm_{}", &OWNER[4..]);
    let (status, last) = command(
        &h.router,
        OWNER,
        "UpdateMembership",
        10,
        json!({ "membership_id": owner_membership, "role": "member" }),
    )
    .await;
    assert_eq!(status, 409, "{last}");
    let (status, last) = command(
        &h.router,
        OWNER,
        "RemoveMember",
        11,
        json!({ "membership_id": owner_membership }),
    )
    .await;
    assert_eq!(status, 409, "{last}");
    // Remove Nova: the tenant membership and the workspace one go; a removed user holds no role.
    let (status, removed) = command(
        &h.router,
        OWNER,
        "RemoveMember",
        12,
        json!({ "membership_id": membership_id }),
    )
    .await;
    assert_eq!(status, 200, "{removed}");
    assert_eq!(removed["result"]["status"], "removed");
    assert_eq!(removed["result"]["workspaces"][0]["status"], "removed");
    let (status, body) = get(&h.router, &nova, "/v1/audit").await;
    forbidden(status, &body, "manage_tenant");
    let (status, twice) = command(
        &h.router,
        OWNER,
        "RemoveMember",
        13,
        json!({ "membership_id": membership_id }),
    )
    .await;
    assert_eq!(status, 409, "{twice}");
    // A workspace membership is workspace administration: the approver is not an admin.
    let viewer_wm = format!("wm_{}", &VIEWER[4..]);
    let (status, body) = command(
        &h.router,
        APPROVER,
        "UpdateMembership",
        14,
        json!({ "membership_id": viewer_wm, "role": "editor" }),
    )
    .await;
    forbidden(status, &body, "manage_workspace");
    let (status, promoted) = command(
        &h.router,
        OWNER,
        "UpdateMembership",
        15,
        json!({ "membership_id": viewer_wm, "role": "editor" }),
    )
    .await;
    assert_eq!(status, 200, "{promoted}");
    assert_eq!(promoted["result"]["workspaces"][0]["role"], "editor");

    // Policies: the owner sets the tenant policy (created, then versioned); a malformed
    // rule is refused whole; the projection shows the current version.
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "fs.write.workspace", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" },
        { "effect_class": "fs.write.host", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" }
    ]);
    let (status, set) = command(
        &h.router,
        OWNER,
        "SetPolicy",
        16,
        json!({ "scope": "tenant", "rules": rules }),
    )
    .await;
    assert_eq!(status, 200, "{set}");
    assert_eq!(set["result"]["version"], 1);
    assert_eq!(set["result"]["rules"].as_array().map_or(0, Vec::len), 3);
    let policy_id = set["result"]["id"].as_str().expect("pol").to_string();
    let (status, set_again) = command(
        &h.router,
        OWNER,
        "SetPolicy",
        17,
        json!({ "scope": "tenant", "rules": rules, "approval_default_ttl_seconds": 600 }),
    )
    .await;
    assert_eq!(status, 200, "{set_again}");
    assert_eq!(
        set_again["result"]["id"], policy_id,
        "the same row, versioned"
    );
    assert_eq!(set_again["result"]["version"], 2);
    let (status, malformed) = command(&h.router, OWNER, "SetPolicy", 18, json!({ "scope": "tenant", "rules": [{ "effect_class": "fs.write.host", "decision": "maybe" }] })).await;
    assert_eq!(status, 400, "{malformed}");
    assert_eq!(malformed["code"], "VALIDATION_SCHEMA");
    let (status, wrong_scope) = command(
        &h.router,
        OWNER,
        "SetPolicy",
        19,
        json!({ "scope": "tenant", "workspace_id": WORKSPACE, "rules": [] }),
    )
    .await;
    assert_eq!(status, 400, "{wrong_scope}");
    let (status, workspace_policy) = command(
        &h.router,
        OWNER,
        "SetPolicy",
        20,
        json!({ "scope": "workspace", "workspace_id": WORKSPACE, "rules": [] }),
    )
    .await;
    assert_eq!(status, 200, "{workspace_policy}");
    let (status, listed) = get(
        &h.router,
        OWNER,
        &format!("/v1/policies?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(status, 200, "{listed}");
    let policies = listed["policies"].as_array().expect("policies");
    assert_eq!(policies.len(), 2, "{listed}");
    assert_eq!(policies[0]["scope"], "tenant");
    assert_eq!(policies[0]["version"], 2);
    assert_eq!(policies[0]["approval_default_ttl_seconds"], 600);
    assert_eq!(policies[1]["scope"], "workspace");

    // Trust decisions: only an approver (or admin) of the request's workspace may decide.
    let (status, posted) = command(
        &h.router,
        OWNER,
        "PostMessage",
        21,
        json!({ "workspace_id": WORKSPACE, "content": format!("write it {APPROVAL_SCENARIO}") }),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    assert_eq!(posted["result"]["turn"]["kind"], "parked", "{posted}");
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let (_, approvals) = get(&h.router, OWNER, &format!("/v1/approvals?run_id={run_id}")).await;
    let request = &approvals["approvals"][0];
    let request_id = request["id"].as_str().expect("apr").to_string();
    let digest = request["params_digest"]
        .as_str()
        .expect("digest")
        .to_string();
    let (status, body) = command(
        &h.router,
        VIEWER,
        "ApproveEffect",
        22,
        json!({ "approval_request_id": request_id, "params_digest": digest }),
    )
    .await;
    forbidden(status, &body, "approve_effect");
    let (status, body) = command(
        &h.router,
        VIEWER,
        "DenyEffect",
        23,
        json!({ "approval_request_id": request_id }),
    )
    .await;
    forbidden(status, &body, "approve_effect");
    let (_, still) = get(&h.router, OWNER, &format!("/v1/approvals?run_id={run_id}")).await;
    assert_eq!(
        still["approvals"][0]["status"], "requested",
        "nothing was decided: {still}"
    );
    let (status, stale) = command(
        &h.router,
        APPROVER,
        "ApproveEffect",
        24,
        json!({ "approval_request_id": request_id, "params_digest": "0".repeat(64) }),
    )
    .await;
    assert_eq!(status, 403, "{stale}");
    assert_eq!(stale["code"], "APPROVAL_SUPERSEDED", "{stale}");
    let (status, granted) = command(
        &h.router,
        APPROVER,
        "ApproveEffect",
        25,
        json!({ "approval_request_id": request_id, "params_digest": digest }),
    )
    .await;
    assert_eq!(status, 200, "{granted}");
    assert_eq!(granted["result"]["turn"]["kind"], "completed", "{granted}");
    // A second request, denied by the approver.
    let (_, posted) = command(
        &h.router,
        OWNER,
        "PostMessage",
        26,
        json!({ "workspace_id": WORKSPACE, "content": format!("write again {APPROVAL_SCENARIO}") }),
    )
    .await;
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let (_, approvals) = get(&h.router, OWNER, &format!("/v1/approvals?run_id={run_id}")).await;
    let denied_id = approvals["approvals"][0]["id"]
        .as_str()
        .expect("apr")
        .to_string();
    let (status, denied) = command(
        &h.router,
        APPROVER,
        "DenyEffect",
        27,
        json!({ "approval_request_id": denied_id }),
    )
    .await;
    assert_eq!(status, 200, "{denied}");

    // Audit: every administrative decision above is in the tenant's chain, in order,
    // with its actor; the chain verifies from genesis; a tampered row breaks it.
    let (status, audit) = get(&h.router, OWNER, "/v1/audit?limit=50").await;
    assert_eq!(status, 200, "{audit}");
    assert_eq!(audit["chain"]["holds"], true, "{audit}");
    let entries = audit["entries"].as_array().expect("entries");
    let mut actions: Vec<String> = entries
        .iter()
        .map(|entry| {
            format!(
                "{}:{}",
                entry["action"].as_str().unwrap_or(""),
                entry["decision"].as_str().unwrap_or("")
            )
        })
        .collect();
    actions.reverse();
    assert_eq!(
        actions,
        vec![
            "InviteMember:applied",
            "UpdateMembership:applied",
            "RemoveMember:applied",
            "UpdateMembership:applied",
            "SetPolicy:applied",
            "SetPolicy:applied",
            "SetPolicy:applied",
            // The first message provisioned the workspace's default teammate (APP-014);
            // that is an audited definition change too.
            "CreateTeammate:applied",
            "ApproveEffect:granted",
            "DenyEffect:denied",
        ],
        "{audit}"
    );
    assert_eq!(
        entries[0]["actor"],
        json!({ "kind": "user", "id": APPROVER }),
        "{audit}"
    );
    assert_eq!(
        entries[1]["actor"],
        json!({ "kind": "user", "id": APPROVER }),
        "{audit}"
    );
    assert_eq!(
        entries[9]["actor"],
        json!({ "kind": "user", "id": OWNER }),
        "{audit}"
    );
    assert_eq!(entries[9]["prev_hash"], "", "genesis");
    assert_eq!(entries[8]["prev_hash"], entries[9]["hash"]);
    assert_eq!(audit["chain"]["entries"], 10);
    let (_, by_workspace) = get(
        &h.router,
        OWNER,
        &format!("/v1/audit?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(
        by_workspace["entries"].as_array().map_or(0, Vec::len),
        5,
        "invite-with-workspace, workspace policy, default teammate, grant, deny: {by_workspace}"
    );
    let tampered = entries[4]["id"].as_str().expect("id").to_string();
    // Append-only (migration 0022): an edit is refused outright, whoever issues it.
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let refused = sqlx::query("UPDATE audit_entries SET reason = 'edited after the fact' WHERE tenant_id = $1 AND id = $2")
        .bind(TENANT)
        .bind(&tampered)
        .execute(&mut *tx)
        .await;
    assert!(refused.is_err(), "an audit entry was edited in place");
    tx.rollback().await.expect("rollback");
    // An attacker with DDL on the table can lift the guard; the chain still names the edit.
    let mut tx = h.pool.begin().await.expect("begin");
    sqlx::query("ALTER TABLE audit_entries DISABLE TRIGGER audit_entries_append_only")
        .execute(&mut *tx)
        .await
        .expect("lift the guard");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("UPDATE audit_entries SET reason = 'edited after the fact' WHERE tenant_id = $1 AND id = $2")
        .bind(TENANT)
        .bind(&tampered)
        .execute(&mut *tx)
        .await
        .expect("tamper");
    sqlx::query("ALTER TABLE audit_entries ENABLE TRIGGER audit_entries_append_only")
        .execute(&mut *tx)
        .await
        .expect("restore the guard");
    tx.commit().await.expect("commit");
    let (_, audit) = get(&h.router, OWNER, "/v1/audit").await;
    assert_eq!(audit["chain"]["holds"], false, "{audit}");
    assert_eq!(audit["chain"]["broken_at"], tampered, "{audit}");

    // Usage and connectors for the owner.
    let (status, usage) = get(
        &h.router,
        OWNER,
        &format!("/v1/usage?workspace_id={WORKSPACE}"),
    )
    .await;
    assert_eq!(status, 200, "{usage}");
    assert!(usage["events_read"].as_u64().is_some_and(|n| n > 0));
    let (status, connectors) = get(&h.router, OWNER, "/v1/connectors").await;
    assert_eq!(status, 200, "{connectors}");
    assert_eq!(connectors["connectors"], json!([]));

    common::drop_pool(&h.pool, &h.name).await;
}

#[tokio::test]
async fn the_web_app_is_served_under_app_with_a_locked_policy_and_no_way_out_of_its_root() {
    let Some(h) = prepare("web_static").await else {
        common::blocked_marker();
        return;
    };
    async fn fetch(router: &axum::Router, path: &str) -> (u16, Vec<(String, String)>, String) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_string()))
            .collect();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
            .await
            .expect("body");
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }
    let header = |headers: &[(String, String)], name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    for path in ["/app", "/app/", "/app/audit", "/app/approvals/apr_1"] {
        let (status, headers, body) = fetch(&h.router, path).await;
        assert_eq!(status, 200, "{path}");
        assert!(body.contains("<div id=\"root\">"), "{path} serves the page");
        assert!(body.contains("viewport"), "{path} is responsive");
        assert_eq!(
            header(&headers, "content-type").as_deref(),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            header(&headers, "content-security-policy").as_deref(),
            Some(WEB_CSP)
        );
        assert_eq!(
            header(&headers, "x-content-type-options").as_deref(),
            Some("nosniff")
        );
        assert_eq!(header(&headers, "x-frame-options").as_deref(), Some("DENY"));
        assert_eq!(
            header(&headers, "cache-control").as_deref(),
            Some("no-cache")
        );
    }
    let (status, headers, body) = fetch(&h.router, "/app/assets/app.js").await;
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "content-type").as_deref(),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        header(&headers, "cache-control").as_deref(),
        Some("no-cache")
    );
    assert_eq!(body, "console.log('web');");
    let (status, _, body) = fetch(&h.router, "/app/assets/app.css").await;
    assert_eq!(status, 200);
    assert!(body.contains("@media"));
    // A missing asset is a typed refusal; a path that would leave the root is never a file.
    for path in [
        "/app/assets/missing.js",
        "/app/../Cargo.toml",
        "/app/assets/../../Cargo.toml",
        "/app/%2e%2e/Cargo.toml",
        "/app/assets/../../../../../etc/hosts",
    ] {
        let (status, _, body) = fetch(&h.router, path).await;
        assert!(status == 404 || status == 400, "{path}: {status} {body}");
        assert!(
            !body.contains("[package]") && !body.contains("localhost"),
            "{path} must not leak a file outside the root"
        );
    }
    // A directory is never listed: it is a client route and gets the page.
    let (status, _, body) = fetch(&h.router, "/app/assets").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("<div id=\"root\">") && !body.contains("app.css"),
        "no listing"
    );
    // Without a root, /app/ is not served at all.
    let bare = quansio_server::api::router(ApiState::new(h.pool.clone()).expect("api"));
    let (status, _, _) = fetch(&bare, "/app/").await;
    assert_eq!(status, 404);
    common::drop_pool(&h.pool, &h.name).await;
}
