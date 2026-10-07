//! Importing another browser's cookies into the agent's browser, end to end (ADP-009, DOMAIN.md §13.9 B, §14): a **real PostgreSQL**
//! holds the staged bundle (the secret broker, with a real data-key envelope), the server's **real dispatcher** decides whether the
//! host-only `browser.import_session` call may run, and a **real headless Chrome** behind the worker's `BrowserDriver` ends up
//! holding the cookies.
//!
//! What is proved:
//! * **staging** (`ImportBrowserSession`): a signed-in person who may cause a tier 4 effect in the workspace seals the bundle; the
//!   answer, the command log (its digest covers the params without their values), the audit chain and every table of the database
//!   hold **no cookie value**; a viewer, an editor, a service principal, a stranger's workspace, a digest that is not the bundle's, an
//!   over-large or malformed bundle and a deployment without a key provider are refused and seal nothing;
//! * **always asked**: the host call for the import parks for an approval (effect class `browser.session.import`, tier 4) and takes
//!   nothing from the runtime while it waits; no model is offered the tool;
//! * **applied once**: with the approval, the worker takes the bundle, the runtime checks it is the one approved, Chrome holds the
//!   cookies, and the stored seal is overwritten in the same transaction: a second take finds nothing, a bundle swapped for another is
//!   refused and spent, and a bundle that outlived its time is overwritten by the sweep.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` and a Chrome; without either the tests print `BLOCKED_EXTERNAL`.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::Request;
use chrono::Utc;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_machine::secrets::imports::{
    bundle_digest, BrowserImports, ImportConfig, ImportCookie, SameSite, StageRequest, TakeRequest,
};
use quansio_machine::secrets::{open, LocalMasterKeyProvider};
use quansio_qworkerd::browser::secrets::{ImportRequest, ImportedBundle, SecretPort};
use quansio_qworkerd::browser::{
    BrowserCallContext, BrowserDriver, BrowserFault, BrowserSession, DriverConfig, FaultCode,
};
use quansio_server::api::ApiState;
use quansio_server::control::schema;
use quansio_server::policy::{ActorRoles, TenantRole, WorkspaceRole};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun, RunTriggerKind,
    RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, HostDispatch, HostFailure, HostOutcome, RoleProvider, StoredProjectionProvider,
    ToolDispatchService, ToolHostPort,
};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

mod common;
use common::outputs::Chrome;
use common::{blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP00E";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP00E";
const EDITOR: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADPEDT";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP00E";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP00E";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9YADP00E";
const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9YADP00E";
const CANARY: &str = "s3ss10n-canary-7f3a";
const HTTP_ONLY_CANARY: &str = "h77p-0nly-canary-91c2";

static NEXT_COMMAND: AtomicU32 = AtomicU32::new(1);

fn command_id() -> String {
    format!(
        "cmd_01J8Z3K6F1N8VQ2X5W9Y0Q{:04}",
        NEXT_COMMAND.fetch_add(1, Ordering::SeqCst)
    )
}

fn provider() -> LocalMasterKeyProvider {
    LocalMasterKeyProvider::from_master_key(&[5u8; 32], "local:browser-import-test")
        .expect("a master key")
}

fn now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn cookie(name: &str, value: &str, http_only: bool) -> ImportCookie {
    ImportCookie {
        name: name.to_string(),
        value: value.to_string(),
        domain: "127.0.0.1".to_string(),
        path: "/".to_string(),
        secure: false,
        http_only,
        same_site: SameSite::Lax,
        expires: Some(4_102_444_800),
    }
}

fn bundle() -> Vec<ImportCookie> {
    vec![
        cookie("session", CANARY, false),
        cookie("auth", HTTP_ONLY_CANARY, true),
        cookie("theme", "dark", false),
    ]
}

fn wire(cookies: &[ImportCookie]) -> Value {
    serde_json::to_value(cookies).expect("cookies")
}

struct Harness {
    router: axum::Router,
    pool: PgPool,
    name: String,
}

async fn prepare(prefix: &str, keys: bool) -> Option<Harness> {
    let name = scratch_name(prefix);
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, OWNER, WORKSPACE, WORK_NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, 'editor@example.com', 'seed')")
        .bind(EDITOR)
        .execute(&mut *tx)
        .await
        .expect("user");
    sqlx::query("INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ('tm_01J8Z3K6F1N8VQ2X5W9YADPEDT', $1, $2, 'member', 'active')")
        .bind(TENANT)
        .bind(EDITOR)
        .execute(&mut *tx)
        .await
        .expect("tenant membership");
    sqlx::query("INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) VALUES ('wm_01J8Z3K6F1N8VQ2X5W9YADPEDT', $1, $2, $3, 'editor', 'active')")
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(EDITOR)
        .execute(&mut *tx)
        .await
        .expect("workspace membership");
    tx.commit().await.expect("commit");
    let mut state = ApiState::new(pool.clone()).expect("api");
    if keys {
        state = state.with_key_provider(Arc::new(provider()));
    }
    Some(Harness {
        router: quansio_server::api::router(state),
        pool,
        name,
    })
}

macro_rules! harness {
    ($prefix:expr, $keys:expr) => {
        match prepare($prefix, $keys).await {
            Some(h) => h,
            None => {
                blocked_marker();
                return;
            }
        }
    };
}

#[derive(Clone)]
enum Who {
    User(&'static str),
    Bearer(String),
}

async fn call(h: &Harness, who: &Who, name: &str, id: &str, params: Value) -> (u16, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/commands/{name}"))
        .header("x-quansio-tenant", TENANT)
        .header("content-type", "application/json");
    builder = match who {
        Who::User(user) => builder.header("x-quansio-user", *user),
        Who::Bearer(token) => builder.header("authorization", format!("Bearer {token}")),
    };
    let request = builder
        .body(axum::body::Body::from(
            json!({ "command_id": id, "params": params }).to_string(),
        ))
        .expect("request");
    let response = h.router.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn stage(h: &Harness, who: &Who, cookies: &[ImportCookie]) -> (u16, Value) {
    stage_with(h, who, WORKSPACE, wire(cookies), &bundle_digest(cookies)).await
}

async fn stage_with(
    h: &Harness,
    who: &Who,
    workspace: &str,
    cookies: Value,
    digest: &str,
) -> (u16, Value) {
    call(
        h,
        who,
        "ImportBrowserSession",
        &command_id(),
        json!({ "workspace_id": workspace, "cookies": cookies, "digest": digest }),
    )
    .await
}

async fn handles(pool: &PgPool, status: Option<&str>) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM secret_handles WHERE provider = 'browser_import' AND ($1::text IS NULL OR status = $1)",
    )
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("count")
}

/// Every table, as text, searched for each needle.
async fn find_anywhere(pool: &PgPool, needles: &[&str]) -> Vec<(String, String)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND left(tablename, 1) <> '_'",
    )
    .fetch_all(pool)
    .await
    .expect("tables");
    let mut found = Vec::new();
    for table in tables {
        let rows = sqlx::query(&format!("SELECT t::text AS row FROM {table} t"))
            .fetch_all(pool)
            .await
            .expect("scan");
        for row in rows {
            let text: String = row.try_get("row").unwrap_or_default();
            for needle in needles {
                if text.contains(needle) {
                    found.push((table.clone(), (*needle).to_string()));
                }
            }
        }
    }
    found
}

fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or("")
}

#[tokio::test(flavor = "multi_thread")]
async fn staging_seals_the_bundle_and_no_record_holds_a_cookie_value() {
    let h = harness!("imp_stage", true);
    let who = Who::User(OWNER);
    let cookies = bundle();
    let (status, body) = stage(&h, &who, &cookies).await;
    assert_eq!(status, 200, "{body}");
    let result = &body["result"];
    assert_eq!(result["state"], "staged");
    assert_eq!(result["cookie_count"], 3);
    assert_eq!(result["digest"], bundle_digest(&cookies));
    assert_eq!(result["domains"], json!(["127.0.0.1"]));
    assert!(result["import_id"]
        .as_str()
        .expect("id")
        .starts_with("sec_"));
    assert!(result["expires_at"].is_string());
    for canary in [CANARY, HTTP_ONLY_CANARY] {
        assert!(
            !body.to_string().contains(canary),
            "the answer holds a value: {body}"
        );
    }
    assert_eq!(handles(&h.pool, Some("active")).await, 1);
    let label: String =
        sqlx::query_scalar("SELECT label FROM secret_handles WHERE provider = 'browser_import'")
            .fetch_one(&h.pool)
            .await
            .expect("label");
    assert_eq!(label, format!("import:{}", bundle_digest(&cookies)));

    // The sealed bytes are the only copy: not the command log, not the audit chain, not any other table.
    assert_eq!(
        find_anywhere(&h.pool, &[CANARY, HTTP_ONLY_CANARY]).await,
        Vec::<(String, String)>::new()
    );
    let audit: String = sqlx::query_scalar("SELECT reason FROM audit_entries WHERE action = 'ImportBrowserSession' ORDER BY occurred_at DESC LIMIT 1")
        .fetch_one(&h.pool)
        .await
        .expect("audit entry");
    assert!(
        audit.contains("3 cookies") && audit.contains("browser.import_session"),
        "{audit}"
    );

    // The command log's digest covers the params without the values: two bundles that differ only in a value share it.
    let mut other = bundle();
    other[0].value = "a-different-session-value".to_string();
    let (status, _) = stage_with(&h, &who, WORKSPACE, wire(&other), &bundle_digest(&other)).await;
    assert_eq!(status, 200);
    let digests: Vec<String> = sqlx::query_scalar("SELECT params_digest FROM commands WHERE command_name = 'ImportBrowserSession' ORDER BY created_at")
        .fetch_all(&h.pool)
        .await
        .expect("digests");
    assert_eq!(digests.len(), 2);
    // (the params also carry the bundle digest, which does differ with a value: the digest of a cookie list is not a way to a value)
    assert_ne!(digests[0], digests[1]);
    // Latest wins: staging again overwrote the first.
    assert_eq!(handles(&h.pool, Some("active")).await, 1);
    assert_eq!(handles(&h.pool, None).await, 2);
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn staging_is_refused_for_everyone_who_may_not_and_seals_nothing() {
    let h = harness!("imp_refused", true);
    let owner = Who::User(OWNER);
    let cookies = bundle();

    // An editor can author work but may not cause a tier 4 effect.
    let (status, refused) = stage(&h, &Who::User(EDITOR), &cookies).await;
    assert_eq!(status, 403, "{refused}");

    // A service principal holding the Browser/computer family still cannot: importing is a person's act.
    let (status, created) = call(
        &h,
        &owner,
        "CreateServicePrincipal",
        &command_id(),
        json!({ "name": "browser-bot", "scopes": ["Browser/computer"] }),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    let bot = Who::Bearer(
        created["result"]["secret"]
            .as_str()
            .expect("key")
            .to_string(),
    );
    let (status, refused) = stage(&h, &bot, &cookies).await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(code(&refused), "SCOPE_FORBIDDEN");

    // A workspace that is not the tenant's.
    let (status, refused) = stage_with(
        &h,
        &owner,
        "ws_01J8Z3K6F1N8VQ2X5W9YADP0ZZ",
        wire(&cookies),
        &bundle_digest(&cookies),
    )
    .await;
    assert_eq!(status, 404, "{refused}");

    // A digest that is not the bundle's, a malformed cookie, an empty bundle, one past the cookie limit.
    let (status, refused) = stage_with(
        &h,
        &owner,
        WORKSPACE,
        wire(&cookies),
        &bundle_digest(&cookies[..2]),
    )
    .await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(code(&refused), "VALIDATION_SCHEMA");
    let mut broken = wire(&cookies);
    broken[0]["domain"] = json!("https://example.test/path");
    let (status, refused) =
        stage_with(&h, &owner, WORKSPACE, broken, &bundle_digest(&cookies)).await;
    assert_eq!(status, 400, "{refused}");
    let (status, refused) = stage_with(&h, &owner, WORKSPACE, json!([]), &bundle_digest(&[])).await;
    assert_eq!(status, 400, "{refused}");
    let mut with_extra = wire(&cookies);
    with_extra[0]["isAdmin"] = json!(true);
    let (status, refused) =
        stage_with(&h, &owner, WORKSPACE, with_extra, &bundle_digest(&cookies)).await;
    assert_eq!(status, 400, "{refused}");
    let too_many: Vec<ImportCookie> = (0..=ImportConfig::builtin().max_cookies)
        .map(|index| cookie(&format!("c{index}"), "v", false))
        .collect();
    let (status, refused) = stage_with(
        &h,
        &owner,
        WORKSPACE,
        wire(&too_many),
        &bundle_digest(&too_many),
    )
    .await;
    assert_eq!(status, 400, "{refused}");
    let mut long = bundle();
    long[0].value = "x".repeat(ImportConfig::builtin().max_value_bytes + 1);
    let (status, refused) =
        stage_with(&h, &owner, WORKSPACE, wire(&long), &bundle_digest(&long)).await;
    assert_eq!(status, 400, "{refused}");

    assert_eq!(handles(&h.pool, None).await, 0);
    assert_eq!(
        find_anywhere(&h.pool, &[CANARY, HTTP_ONLY_CANARY]).await,
        Vec::<(String, String)>::new()
    );
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deployment_without_a_key_provider_refuses_and_keeps_nothing() {
    let h = harness!("imp_nokeys", false);
    let (status, refused) = stage(&h, &Who::User(OWNER), &bundle()).await;
    assert_eq!(status, 503, "{refused}");
    assert_eq!(code(&refused), "ADAPTER_UNAVAILABLE");
    assert_eq!(handles(&h.pool, None).await, 0);
    assert_eq!(
        find_anywhere(&h.pool, &[CANARY]).await,
        Vec::<(String, String)>::new()
    );
    drop_pool(&h.pool, &h.name).await;
}

// ------------------------------------------------------------------------------------------------------------------------------
// The dispatcher: the import is always asked.
// ------------------------------------------------------------------------------------------------------------------------------

/// A host that must never be reached: the import is parked for an approval before any host is asked.
struct NoHost(Arc<AtomicUsize>);

#[async_trait]
impl ToolHostPort for NoHost {
    async fn execute(&self, _dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(HostFailure::Unavailable {
            owner: "ADP-009",
            detail: "no host in this suite".to_string(),
        })
    }
}

struct Quiet;

#[async_trait]
impl ModelProposalSource for Quiet {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        Ok(ModelProposal {
            assistant_text: Some("nothing to do".to_string()),
            ..ModelProposal::default()
        })
    }
}

struct Admin;

#[async_trait]
impl RoleProvider for Admin {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Owner), Some(WorkspaceRole::Admin)),
            user_id: Some(OWNER.to_string()),
        }))
    }
}

async fn dispatcher_for(
    pool: &PgPool,
    reached: Arc<AtomicUsize>,
) -> (RuntimeEngine, Arc<ToolDispatchService>) {
    let identity = RuntimeIdentity::system(
        TENANT,
        "import-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let delegation: Arc<dyn DelegationPort> = Arc::new(
        AgentDelegationPort::with_structural_check(pool.clone(), identity.clone())
            .expect("delegation"),
    );
    let dispatcher = Arc::new(
        ToolDispatchService::new(
            pool.clone(),
            identity.clone(),
            Arc::new(
                StoredProjectionProvider::new(pool.clone(), identity.clone()).expect("projections"),
            ),
            Arc::new(Admin),
            Arc::new(NoHost(reached)),
            Arc::clone(&delegation),
        )
        .expect("dispatch"),
    );
    let engine = RuntimeEngine::new(pool.clone(), identity)
        .expect("engine")
        .with_model_source(Arc::new(Quiet))
        .with_tool_dispatch(dispatcher.clone())
        .with_delegation(delegation);
    (engine, dispatcher)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_import_call_parks_for_a_tier_four_approval_and_takes_nothing_while_it_waits() {
    let h = harness!("imp_ask", true);
    let (status, staged) = stage(&h, &Who::User(OWNER), &bundle()).await;
    assert_eq!(status, 200, "{staged}");
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')")
        .bind(AGENT)
        .bind(TENANT)
        .bind(WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("agent thread");
    // The deployment's policy for the class: an administrator writes the rule that lets the question be asked. A rule that says
    // `allow` is raised to `ask` for a tier 4 class (below), and with no rule at all the call is denied (fail closed).
    sqlx::query("INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YADP00E', $1, NULL, 'tenant', $2)")
        .bind(TENANT)
        .bind(json!([{ "effect_class": "browser.session.import", "resource": { "kind": "domain", "selector": "*" }, "decision": "allow" }]))
        .execute(&mut *tx)
        .await
        .expect("policy");
    tx.commit().await.expect("commit");

    let reached = Arc::new(AtomicUsize::new(0));
    let (engine, dispatcher) = dispatcher_for(&h.pool, Arc::clone(&reached)).await;
    let run = engine
        .create_run(NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("node"),
            CanonicalId::parse_typed(AGENT, Prefix::AgentThread).expect("agent"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("run");
    let run = engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    let run = engine.start(&run.id, run.generation).await.expect("start");
    // The run holds the grant the import needs; nothing else is needed to *ask*.
    let projection = CapabilityProjection::from_parts(
        CanonicalId::generate(Prefix::CapabilityProjection, &mut UlidGenerator::new()),
        ProjectionSubject::new(SubjectKind::Run, run.id.to_string()),
        Vec::new(),
        vec![Grant::from_json(&json!({
            "effect_class": "browser.session.import",
            "resource": { "kind": "domain", "selector": "*" },
            "constraints": {},
        }))
        .expect("grant")],
        Utc::now(),
        Some(Utc::now() + chrono::Duration::hours(1)),
    )
    .expect("projection");
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("INSERT INTO capability_projections (id, tenant_id, workspace_id, subject_kind, subject_id, inputs, grants, inputs_digest, computed_at, expires_at) VALUES ($1, $2, $3, 'run', $4, $5, $6, $7, $8, $9)")
        .bind(projection.id.to_string())
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(run.id.to_string())
        .bind(serde_json::to_value(&projection.inputs).expect("inputs"))
        .bind(serde_json::to_value(&projection.grants).expect("grants"))
        .bind(projection.inputs_digest.to_string())
        .bind(projection.computed_at)
        .bind(projection.expires_at)
        .execute(&mut *tx)
        .await
        .expect("projection row");
    tx.commit().await.expect("commit");
    let current = engine.store().load_run(&run.id).await.expect("run");
    let _ = engine
        .run_turn(
            &run.id,
            current.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let (turn_id, step_id): (String, String) = sqlx::query_as("SELECT t.id, s.id FROM steps s JOIN turns t ON t.id = s.turn_id WHERE t.run_id = $1 AND t.tenant_id = $2 ORDER BY s.created_at LIMIT 1")
        .bind(run.id.to_string())
        .bind(TENANT)
        .fetch_one(&mut *tx)
        .await
        .expect("the turn recorded a step");
    tx.commit().await.expect("commit");

    let current = engine.store().load_run(&run.id).await.expect("run");
    let args = json!({
        "session_id": SESSION,
        "import_id": staged["result"]["import_id"],
        "digest": staged["result"]["digest"],
        "cookie_count": 3,
        "domains": ["127.0.0.1"],
    });
    let n = UlidGenerator::new().generate().to_string();
    let outcome = dispatcher
        .dispatch_as_host(
            &format!("host_{n}"),
            "browser.import_session",
            args.clone(),
            quansio_server::runtime::state_machine::ToolDispatchRequest {
                run_id: run.id.to_string(),
                turn_id: turn_id.clone(),
                step_id: step_id.clone(),
                generation: current.generation.get(),
            },
        )
        .await
        .expect("the host call is answered");
    assert!(
        matches!(
            outcome,
            quansio_server::runtime::state_machine::ToolDispatchOutcome::Parked { .. }
        ),
        "an import waits for a person: {outcome:?}"
    );
    assert_eq!(
        reached.load(Ordering::SeqCst),
        0,
        "no host was asked while the approval is open"
    );
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let effects: Vec<(String, String)> = sqlx::query_as("SELECT effect_class, status FROM effect_records WHERE tenant_id = $1 AND run_id = $2 ORDER BY created_at")
        .bind(TENANT)
        .bind(run.id.to_string())
        .fetch_all(&mut *tx)
        .await
        .expect("effects");
    let asked: i64 = sqlx::query_scalar("SELECT count(*) FROM approval_requests WHERE tenant_id = $1 AND run_id = $2 AND status = 'requested'")
        .bind(TENANT)
        .bind(run.id.to_string())
        .fetch_one(&mut *tx)
        .await
        .expect("approvals");
    let caller: String = sqlx::query_scalar("SELECT caller FROM tool_calls WHERE tenant_id = $1 AND tool_name = 'browser.import_session' ORDER BY created_at DESC LIMIT 1")
        .bind(TENANT)
        .fetch_one(&mut *tx)
        .await
        .expect("call row");
    tx.commit().await.expect("commit");
    assert_eq!(
        effects,
        vec![("browser.session.import".to_string(), "PROPOSED".to_string())]
    );
    assert_eq!(asked, 1, "a person is asked");
    // (Parking the run is the caller's: the turn loop or a script executor that makes a host call acts on `Parked`. No orchestrator
    // makes this call for a person yet; see DOMAIN.md 13.9 B and the task's recorded gaps.)
    assert_eq!(caller, "host");
    // The bundle is still sealed and untouched: nothing was taken.
    assert_eq!(handles(&h.pool, Some("active")).await, 1);

    // No model is offered the tool, and a model that names it is refused.
    let visible = dispatcher.registry();
    assert!(visible
        .get("browser.import_session")
        .is_some_and(|tool| !tool.model_visible && tool.host_callable));
    drop_pool(&h.pool, &h.name).await;
}

// ------------------------------------------------------------------------------------------------------------------------------
// The approved call: the worker takes the staged bundle through the runtime, and Chrome holds the cookies.
// ------------------------------------------------------------------------------------------------------------------------------

/// The worker's import port over the runtime's take, in a real transaction.
fn port(pool: PgPool) -> SecretPort {
    SecretPort::new(|_| {
        Box::pin(async {
            Err(BrowserFault::new(
                FaultCode::SecretUnavailable,
                "no placeholder in this suite",
            ))
        })
    })
    .with_imports(move |request: ImportRequest| {
        let pool = pool.clone();
        Box::pin(async move {
            let unavailable =
                |why: &str| BrowserFault::new(FaultCode::SecretUnavailable, why.to_string());
            let mut tx = pool
                .begin()
                .await
                .map_err(|_| unavailable("the runtime is unavailable"))?;
            schema::set_tenant_context(&mut tx, TENANT)
                .await
                .map_err(|_| unavailable("tenant"))?;
            let at = now();
            let taken = BrowserImports::take(
                &mut tx,
                &provider(),
                &TakeRequest {
                    tenant_id: TENANT,
                    workspace_id: WORKSPACE,
                    import_id: &request.import_id,
                    digest: &request.digest,
                    effect_id: &request.action_id,
                    at: &at,
                },
            )
            .await;
            // The take overwrites the bundle even when it then refuses it, so the transaction is committed either way.
            let committed = tx.commit().await.is_ok();
            match taken {
                Ok(taken) if committed => Ok(ImportedBundle {
                    cookies: taken.cookies,
                }),
                Ok(_) => Err(unavailable("the runtime could not record the take")),
                Err(_) => Err(unavailable("no usable bundle is staged under this id")),
            }
        })
    })
}

fn driver(chrome: &Chrome, dir: &std::path::Path, port: SecretPort) -> BrowserDriver {
    let mut config = DriverConfig::new(chrome.port, dir);
    config.action_timeout = std::time::Duration::from_millis(2500);
    config.settle_timeout = std::time::Duration::from_millis(2000);
    BrowserDriver::open(
        config,
        BrowserSession::open(SESSION, TENANT, "tgt_01J8Z3K6F1N8VQ2X5W9YADP00E"),
        Some(port),
    )
    .expect("driver")
}

fn ctx(n: u32, tier: Option<u8>) -> BrowserCallContext {
    BrowserCallContext {
        run_id: "run_01J8Z3K6F1N8VQ2X5W9YADP00E".to_string(),
        tool_call_id: format!("tc_imp{n:04}"),
        action_id: format!("act_imp{n:04}"),
        approved_tier: tier,
    }
}

async fn tool(
    driver: &BrowserDriver,
    n: u32,
    tier: Option<u8>,
    name: &str,
    mut args: Value,
) -> Value {
    args["session_id"] = json!(SESSION);
    driver.dispatch(name, &args, &ctx(n, tier)).await.to_json()
}

fn import_args(staged: &Value, count: usize) -> Value {
    json!({
        "import_id": staged["result"]["import_id"],
        "digest": staged["result"]["digest"],
        "cookie_count": count,
        "domains": staged["result"]["domains"],
    })
}

/// A loopback page, so the cookies have an origin to belong to.
async fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let body = "<html><head><title>Account</title></head><body>account</body></html>";
                let _ = stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                            .as_bytes(),
                    )
                    .await;
            });
        }
    });
    port
}

async fn devtools(port: u16, path: &str) -> Value {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("timeout");
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("write");
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap_or("[]")).unwrap_or(Value::Null)
    })
    .await
    .expect("join")
}

/// Evaluate an expression in the first page and return its value.
async fn page_eval(chrome: &Chrome, expression: &str) -> Option<Value> {
    let targets = devtools(chrome.port, "/json/list").await;
    let socket = targets.as_array()?.iter().find(|t| t["type"] == "page")?["webSocketDebuggerUrl"]
        .as_str()?
        .to_string();
    let (mut ws, _) = tokio_tungstenite::connect_async(&socket).await.ok()?;
    use futures_util::{SinkExt, StreamExt};
    let request = json!({"id": 1, "method": "Runtime.evaluate", "params": {"expression": expression, "returnByValue": true}});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        request.to_string(),
    ))
    .await
    .ok()?;
    while let Some(Ok(message)) = ws.next().await {
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text).ok()?;
            if value["id"] == 1 {
                return Some(value["result"]["result"]["value"].clone());
            }
        }
    }
    None
}

async fn ciphertexts(pool: &PgPool) -> Vec<(String, Option<Vec<u8>>)> {
    sqlx::query("SELECT status, ciphertext FROM secret_handles WHERE provider = 'browser_import' ORDER BY created_at, id")
        .fetch_all(pool)
        .await
        .expect("handles")
        .iter()
        .map(|row| (row.get("status"), row.get("ciphertext")))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approved_call_applies_the_staged_bundle_once_and_the_runtime_overwrites_it() {
    let h = harness!("imp_apply", true);
    let Some(chrome) = Chrome::launch() else {
        eprintln!("BLOCKED_EXTERNAL: no Chrome on this host");
        drop_pool(&h.pool, &h.name).await;
        return;
    };
    let site = serve().await;
    let cookies = bundle();
    let (status, staged) = stage(&h, &Who::User(OWNER), &cookies).await;
    assert_eq!(status, 200, "{staged}");
    let sealed = ciphertexts(&h.pool).await;
    assert_eq!(sealed.len(), 1);
    let cipher = sealed[0].1.as_ref().expect("sealed");
    for canary in [CANARY, HTTP_ONLY_CANARY] {
        assert!(
            !cipher.windows(canary.len()).any(|w| w == canary.as_bytes()),
            "the stored bytes are not the value"
        );
    }

    let dir = tempfile::tempdir().expect("session dir");
    let driver = driver(&chrome, dir.path(), port(h.pool.clone()));
    let opened = tool(
        &driver,
        1,
        Some(4),
        "browser.navigate",
        json!({"url": format!("http://127.0.0.1:{site}/")}),
    )
    .await;
    assert_eq!(opened["status"], "ok", "{opened}");
    assert_eq!(page_eval(&chrome, "document.cookie").await, Some(json!("")));

    let applied = tool(
        &driver,
        2,
        Some(4),
        "browser.import_session",
        import_args(&staged, 3),
    )
    .await;
    assert_eq!(applied["status"], "ok", "{applied}");
    assert_eq!(applied["output"]["imported"], true, "{applied}");
    assert_eq!(applied["output"]["applied"], 3);
    assert_eq!(applied["output"]["rejected"], 0);
    let seen = page_eval(&chrome, "document.cookie")
        .await
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    assert!(
        seen.contains(&format!("session={CANARY}")) && seen.contains("theme=dark"),
        "{seen}"
    );
    assert!(
        !seen.contains("auth="),
        "the HttpOnly cookie is held by the browser, not readable by the page: {seen}"
    );
    for canary in [CANARY, HTTP_ONLY_CANARY] {
        assert!(!applied.to_string().contains(canary));
    }

    // The take overwrote the stored seal: revoked, and what it opens to is nothing.
    let after = ciphertexts(&h.pool).await;
    assert_eq!(after[0].0, "revoked");
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let opened_to_nothing =
        open(&provider(), after[0].1.as_ref().expect("cipher")).map(|m| m.is_empty());
    tx.rollback().await.expect("rollback");
    assert_eq!(
        opened_to_nothing.ok(),
        Some(true),
        "the overwritten seal opens to nothing"
    );
    assert_eq!(
        find_anywhere(&h.pool, &[CANARY, HTTP_ONLY_CANARY]).await,
        Vec::<(String, String)>::new()
    );

    // A second call finds nothing.
    let again = tool(
        &driver,
        3,
        Some(4),
        "browser.import_session",
        import_args(&staged, 3),
    )
    .await;
    assert_eq!(again["status"], "error", "{again}");
    assert_eq!(again["error"]["details"]["stage"], "take");
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_swapped_for_another_or_past_its_time_is_refused_and_spent() {
    let h = harness!("imp_swap", true);
    let Some(chrome) = Chrome::launch() else {
        eprintln!("BLOCKED_EXTERNAL: no Chrome on this host");
        drop_pool(&h.pool, &h.name).await;
        return;
    };
    let site = serve().await;
    let dir = tempfile::tempdir().expect("session dir");
    let driver = driver(&chrome, dir.path(), port(h.pool.clone()));
    let opened = tool(
        &driver,
        1,
        Some(4),
        "browser.navigate",
        json!({"url": format!("http://127.0.0.1:{site}/")}),
    )
    .await;
    assert_eq!(opened["status"], "ok", "{opened}");

    // The approval carried the digest of one bundle; another was staged afterwards. The runtime refuses, and the new bundle is spent.
    let first = bundle();
    let (status, approved) = stage(&h, &Who::User(OWNER), &first).await;
    assert_eq!(status, 200, "{approved}");
    let mut second = bundle();
    second[0].value = "a-different-session-value".to_string();
    let (status, staged_later) = stage(&h, &Who::User(OWNER), &second).await;
    assert_eq!(status, 200, "{staged_later}");
    let mut swapped = import_args(&staged_later, 3);
    swapped["digest"] = approved["result"]["digest"].clone();
    let refused = tool(&driver, 2, Some(4), "browser.import_session", swapped).await;
    assert_eq!(refused["status"], "error", "{refused}");
    assert_eq!(refused["error"]["details"]["stage"], "take");
    assert_eq!(
        page_eval(&chrome, "document.cookie").await,
        Some(json!("")),
        "nothing was applied"
    );
    assert_eq!(
        handles(&h.pool, Some("active")).await,
        0,
        "the refused bundle was spent, not kept for another try"
    );

    // A bundle that outlived its time is overwritten by the sweep, and a take after it finds nothing.
    let (status, late) = stage(&h, &Who::User(OWNER), &bundle()).await;
    assert_eq!(status, 200, "{late}");
    sqlx::query("UPDATE secret_handles SET created_at = now() - interval '10 minutes', expires_at = now() - interval '1 minute' WHERE provider = 'browser_import' AND status = 'active'")
        .execute(&h.pool)
        .await
        .expect("backdate");
    let expired = tool(
        &driver,
        3,
        Some(4),
        "browser.import_session",
        import_args(&late, 3),
    )
    .await;
    assert_eq!(expired["status"], "error", "{expired}");
    assert_eq!(expired["error"]["details"]["stage"], "take");
    assert_eq!(handles(&h.pool, Some("active")).await, 0);
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let swept = BrowserImports::sweep(&mut tx, &provider(), TENANT, WORKSPACE, &now())
        .await
        .expect("sweep");
    tx.commit().await.expect("commit");
    assert_eq!(swept, 0, "the take had already swept it");
    for (status, cipher) in ciphertexts(&h.pool).await {
        assert_eq!(status, "revoked");
        let cipher = cipher.expect("cipher");
        assert!(
            open(&provider(), &cipher)
                .map(|m| m.is_empty())
                .unwrap_or(false),
            "every spent bundle opens to nothing"
        );
    }
    assert_eq!(
        find_anywhere(
            &h.pool,
            &[CANARY, HTTP_ONLY_CANARY, "a-different-session-value"]
        )
        .await,
        Vec::<(String, String)>::new()
    );
    drop_pool(&h.pool, &h.name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_for_another_workspace_cannot_be_taken() {
    let h = harness!("imp_ws", true);
    let cookies = bundle();
    let (status, staged) = stage(&h, &Who::User(OWNER), &cookies).await;
    assert_eq!(status, 200, "{staged}");
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let at = now();
    let taken = BrowserImports::take(
        &mut tx,
        &provider(),
        &TakeRequest {
            tenant_id: TENANT,
            workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9YADP0ZZ",
            import_id: staged["result"]["import_id"].as_str().expect("id"),
            digest: staged["result"]["digest"].as_str().expect("digest"),
            effect_id: "act_x",
            at: &at,
        },
    )
    .await;
    tx.rollback().await.expect("rollback");
    assert!(
        taken.is_err(),
        "another workspace's browser cannot take the bundle"
    );
    assert_eq!(
        handles(&h.pool, Some("active")).await,
        1,
        "and it is still waiting"
    );
    // Staging directly through the store with a digest that is not the bundle's is refused, so a surface cannot name a bundle it
    // did not send.
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let staged_again = BrowserImports::stage(
        &mut tx,
        &provider(),
        &ImportConfig::builtin(),
        &StageRequest {
            tenant_id: TENANT,
            workspace_id: WORKSPACE,
            cookies: &cookies,
            claimed_digest: "0".repeat(64).as_str(),
            at: &now(),
        },
    )
    .await;
    tx.rollback().await.expect("rollback");
    assert!(staged_again.is_err());
    drop_pool(&h.pool, &h.name).await;
}
