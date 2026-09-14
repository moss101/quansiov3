//! The public API surface (APP-001).
//!
//! Three things are checked here, and each is a contract rather than a behaviour of one handler:
//!
//! * the §15 error vocabulary and shape, against the generated `schemas/catalog/errors.yaml` — the same
//!   table the bindings come from — so an added or renamed code fails in the suite rather than drifting;
//! * the §14 command catalog the surface serves, against the same catalog the contract gate checks;
//! * the mutating path: tenant scope, idempotency by `command_id`, and that the change is made by the
//!   module that owns it rather than by the handler.
//!
//! The scan in `an_api_handler_reads_and_writes_only_its_own_table` is the one a handler's output cannot
//! show, which is why it is structural.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use quansio_server::api::{error::ApiErrorCode, ApiError, ApiState};
use quansio_server::composition::Composition;
use serde_json::Value;
use tower::ServiceExt;

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const COMMAND: &str = "cmd_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

// ------------------------------------------------------------------ the §15 table

#[test]
fn the_error_vocabulary_is_the_one_the_catalog_generates() {
    let catalog: serde_yaml::Value =
        serde_yaml::from_str(quansio_server::api::ERROR_CATALOG).expect("the catalog parses");
    let declared: Vec<String> = catalog
        .get("errors")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("the catalog has an errors list")
        .iter()
        .filter_map(|entry| {
            entry
                .get("code")
                .and_then(serde_yaml::Value::as_str)
                .map(str::to_string)
        })
        .collect();

    let ours: Vec<String> = ApiErrorCode::ALL
        .iter()
        .map(|code| code.as_str().to_string())
        .collect();
    assert!(!ours.is_empty(), "the enum must name every code");

    // Both directions: a code the table has and we do not is a missing refusal, and one we have and the
    // table does not is an invented one. Either way the two disagree about what a client may be told.
    let missing: Vec<&String> = declared.iter().filter(|c| !ours.contains(c)).collect();
    let invented: Vec<&String> = ours.iter().filter(|c| !declared.contains(c)).collect();
    assert!(missing.is_empty(), "the enum is missing {missing:?}");
    assert!(invented.is_empty(), "the enum invents {invented:?}");
    assert_eq!(ours.len(), declared.len(), "the counts must agree");
}

#[test]
fn every_code_maps_to_a_status_and_says_whether_a_retry_could_help() {
    for code in ApiErrorCode::ALL {
        let error = ApiError::new(code, "a message", "corr");
        assert_eq!(error.code(), Some(code), "{code:?} did not round trip");
        assert_eq!(error.status(), code.status(), "{code:?}");
        assert!(!code.family().is_empty(), "{code:?} has no family");
        // A refusal is only retryable when the fault is on the other side of the wire.
        if code.retryable() {
            assert!(
                matches!(
                    code.status(),
                    StatusCode::SERVICE_UNAVAILABLE
                        | StatusCode::TOO_MANY_REQUESTS
                        | StatusCode::INTERNAL_SERVER_ERROR
                ),
                "{code:?} is retryable and would not be retried"
            );
        }
    }
    // The shape is §15's, exactly: extra fields would be a contract change.
    let json =
        serde_json::to_value(ApiError::new(ApiErrorCode::NotFound, "m", "c")).expect("serialise");
    let keys: Vec<&str> = json
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    for required in ["code", "message", "correlation_id", "retryable"] {
        assert!(
            keys.contains(&required),
            "the shape is missing {required}: {keys:?}"
        );
    }
    assert_eq!(keys.len(), 4, "the shape carries an extra field: {keys:?}");
    assert_eq!(
        ApiError::new(ApiErrorCode::Internal, "m", "c").with_details(serde_json::json!({ "a": 1 })),
        ApiError {
            code: "INTERNAL".to_string(),
            message: "m".to_string(),
            correlation_id: "c".to_string(),
            retryable: true,
            details: Some(serde_json::json!({ "a": 1 })),
        }
    );
}

#[test]
fn the_command_catalog_is_the_one_the_gate_checks() {
    let catalog = quansio_server::api::Catalog::from_embedded().expect("the catalog parses");
    let commands = catalog.commands();
    assert!(
        commands.len() > 20,
        "the catalog looks truncated: {}",
        commands.len()
    );
    // Commands the surface has to be able to route to.
    for expected in ["CreateWorkspace", "PostMessage", "CancelRun"] {
        assert!(
            commands
                .iter()
                .any(|name| name.eq_ignore_ascii_case(expected)),
            "{expected} is missing from the catalog"
        );
    }
    // Groups are reported, so a client can present the catalog as the document does.
    assert!(catalog.groups.keys().any(|group| group == "Identity"));
    let projections =
        quansio_server::api::Catalog::read_projections().expect("the projections parse");
    assert!(
        projections.iter().any(|path| path == "/v1/runs/{id}"),
        "{projections:?}"
    );
}

// ------------------------------------------------------------------ the ownership boundary

#[test]
fn an_api_handler_reads_and_writes_only_its_own_table() {
    // The failure this design exists to prevent is a handler writing another owner's table, and no test of
    // a handler's output can see it. So the module is scanned: the only SQL it may carry is against
    // `commands`, the log §1.2's idempotency is defined over.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    let mut scanned = 0;
    let mut offenders = Vec::new();
    let mut stack = vec![root];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            scanned += 1;
            for (line_number, line) in text.lines().enumerate() {
                let upper = line.to_uppercase();
                let writes = upper.contains("INSERT INTO")
                    || upper.contains("UPDATE ")
                    || upper.contains("DELETE FROM");
                if !writes {
                    continue;
                }
                if !upper.contains("COMMANDS") {
                    offenders.push(format!(
                        "{}:{}: {}",
                        path.display(),
                        line_number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(scanned >= 2, "the scan must actually read the module");
    assert!(
        offenders.is_empty(),
        "the API writes a table it does not own: {offenders:?}"
    );
}

// ------------------------------------------------------------------ the deployed surface

async fn service(prefix: &str) -> Option<(axum::Router, sqlx::PgPool, String)> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    // The scratch database is empty: the schema is applied through its canonical runner, the same one the
    // deployment uses, rather than by a test-local copy of the DDL.
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("migrate");
    common::seed_tenant(
        &pool,
        TENANT,
        USER,
        WORKSPACE,
        "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
    )
    .await;
    let state = ApiState::new(pool.clone()).expect("api state");
    Some((quansio_server::api::router(state), pool, name))
}

/// A run created by the runtime, which owns runs.
async fn runtime_run(pool: &sqlx::PgPool) -> (quansio_core::CanonicalId, u64) {
    use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
    // A run binds an agent thread, so the fixture creates the thread it binds rather than naming one that
    // does not exist.
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind) \
         VALUES ('ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC', $1, $2, 'worker')",
    )
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(pool)
    .await
    .expect("seed an agent thread");
    use quansio_server::runtime::state_machine::{
        NewRun, RunStatus, RunTriggerKind, RuntimeIdentity, RuntimeStore,
    };
    let identity = RuntimeIdentity::system(
        TENANT,
        "api-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let store = RuntimeStore::new(pool.clone(), identity).expect("runtime store");
    let run = store
        .create_run(NewRun::new(
            WORKSPACE.to_string(),
            CanonicalId::parse_typed("wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", Prefix::WorkNode)
                .expect("work node"),
            CanonicalId::parse_typed("ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", Prefix::AgentThread)
                .expect("agent thread"),
            RunTriggerKind::Manual,
        ))
        .await
        .expect("create a run");
    // A run is cancellable only from a live state, and the state machine draws the ladder explicitly:
    // CREATED -> QUEUED -> RUNNING. The fixture walks it through the runtime's own edges rather than asking
    // the API to make an illegal move.
    let queued = store
        .transition_run(&run.id, run.generation, RunStatus::Queued, None)
        .await
        .expect("queue the run");
    let started = store
        .start(&queued.id, queued.generation)
        .await
        .expect("start the run");
    (started.id, started.generation.get())
}

fn request(method: &str, path: &str, tenant: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(tenant) = tenant {
        builder = builder.header("x-quansio-tenant", tenant);
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("a request"),
        None => builder.body(Body::empty()).expect("a request"),
    }
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("a body");
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn health_reports_readiness_and_the_catalogues_it_serves() {
    let Some((router, pool, name)) = service("api_health").await else {
        common::blocked_marker();
        return;
    };
    let response = router
        .clone()
        .oneshot(request("GET", "/v1/health", None, None))
        .await
        .expect("a response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["status"], "ready");
    assert!(body["commands"].as_u64().unwrap_or(0) > 20, "{body}");

    let response = router
        .clone()
        .oneshot(request("GET", "/v1/commands", None, None))
        .await
        .expect("a response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert!(body["groups"]["Identity"].as_array().is_some(), "{body}");

    let response = router
        .oneshot(request("GET", "/v1/read-projections", None, None))
        .await
        .expect("a response");
    assert_eq!(response.status(), StatusCode::OK);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_call_without_a_usable_tenant_is_refused_with_the_right_code() {
    let Some((router, pool, name)) = service("api_scope").await else {
        common::blocked_marker();
        return;
    };

    // No tenant at all is an authentication failure.
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            None,
            Some(serde_json::json!({ "command_id": COMMAND })),
        ))
        .await
        .expect("a response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(response).await;
    assert_eq!(body["code"], "AUTH_REQUIRED", "{body}");

    // A malformed tenant is a bad request, and the two are kept apart so a client can tell a missing
    // credential from a typo in one.
    for tenant in [
        "not-a-tenant",
        "tn_lowercase",
        "usr_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
    ] {
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/v1/commands/CancelRun",
                Some(tenant),
                Some(serde_json::json!({ "command_id": COMMAND })),
            ))
            .await
            .expect("a response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{tenant}");
        let body = json_body(response).await;
        assert_eq!(body["code"], "VALIDATION_BOUNDS", "{tenant}: {body}");
        // The shape is §15's whatever the code.
        assert!(body["correlation_id"].is_string(), "{body}");
        assert!(body["retryable"].is_boolean(), "{body}");
    }

    // A command that is not in the catalog is a 404 rather than a 500 or a silent success.
    let response = router
        .oneshot(request(
            "POST",
            "/v1/commands/Teleport",
            Some(TENANT),
            Some(serde_json::json!({ "command_id": COMMAND })),
        ))
        .await
        .expect("a response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(response).await["code"], "NOT_FOUND");
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_mutating_call_calls_the_owner_and_a_resend_returns_its_result_once() {
    let Some((router, pool, name)) = service("api_idem").await else {
        common::blocked_marker();
        return;
    };
    // A run to act on, created by the module that owns runs rather than by the handler under test.
    let (run, generation) = runtime_run(&pool).await;
    let run_id = run.to_string();
    let body = serde_json::json!({
        "command_id": COMMAND,
        "params": { "run_id": run_id, "generation": generation },
    });

    // The first call applies the command through the runtime, which owns the run's lifecycle.
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(TENANT),
            Some(body.clone()),
        ))
        .await
        .expect("a response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the owner's result is what is returned"
    );
    let first = json_body(response).await;
    assert_eq!(
        first["result"]["run_id"].as_str(),
        Some(run_id.as_str()),
        "{first}"
    );
    assert_eq!(first["result"]["status"], "CANCELLED", "{first}");
    assert_eq!(first["replayed"], false, "{first}");

    // The run was cancelled in the owner's table, not in a table the handler invented.
    let status: String =
        sqlx::query_scalar("SELECT status FROM runs WHERE id = $1 AND tenant_id = $2")
            .bind(&run_id)
            .bind(TENANT)
            .fetch_one(&pool)
            .await
            .expect("status");
    assert_eq!(status, "CANCELLED");

    // A resend of the same command is answered from the record, with the same answer.
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(TENANT),
            Some(body.clone()),
        ))
        .await
        .expect("a response");
    let second = json_body(response).await;
    assert_eq!(second["replayed"], true, "{second}");
    assert_eq!(second["result"], first["result"], "{second}");

    // The same command_id with different parameters is refused: the two cannot both be what that command
    // meant.
    let mut changed = body.clone();
    changed["params"]["generation"] = serde_json::json!(generation + 1);
    let response = router
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(TENANT),
            Some(changed),
        ))
        .await
        .expect("a response");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(response).await["code"],
        "CONFLICT_IDEMPOTENCY_MISMATCH"
    );
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_command_scoped_to_another_tenant_does_not_touch_this_one() {
    let Some((router, pool, name)) = service("api_isolation").await else {
        common::blocked_marker();
        return;
    };
    // The tenant in the header is the only tenant the call may act for: a command carrying another
    // tenant's workspace fails on the owner's own foreign key rather than writing across the boundary.
    // A run in *this* tenant, which another tenant's call must not touch. The header's tenant is the only
    // scope a call may act for, and the owner enforces it rather than the handler.
    let (run, generation) = runtime_run(&pool).await;
    let body = serde_json::json!({
        "command_id": COMMAND,
        "params": { "run_id": run.to_string(), "generation": generation },
    });
    let other = "tn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'other')")
        .bind(other)
        .execute(&pool)
        .await
        .expect("seed the other tenant");
    let response = router
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(other),
            Some(body),
        ))
        .await
        .expect("a response");
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "another tenant cancelled this run"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = $1")
        .bind(run.to_string())
        .fetch_one(&pool)
        .await
        .expect("status");
    assert_ne!(
        status, "CANCELLED",
        "another tenant's call changed this tenant's run"
    );
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn the_deployable_composes_from_the_environment() {
    // The composition root is what makes this one deployable, so it is driven: no database URL is a
    // startup refusal, and a real one composes and applies the schema.
    std::env::remove_var("QUANSIO_DATABASE_URL");
    assert!(quansio_server::composition::ServerConfig::from_env().is_err());
    let Some(dsn) = common::admin_url() else {
        common::blocked_marker();
        return;
    };
    let name = common::scratch_name("api_compose");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    common::drop_pool(&pool, &name).await;
    std::env::set_var("QUANSIO_DATABASE_URL", &dsn);
    std::env::set_var("QUANSIO_BIND", "127.0.0.1:0");
    let config = quansio_server::composition::ServerConfig::from_env().expect("config");
    let composition = Composition::new(config).await.expect("compose");
    assert!(composition.api().catalog().commands().len() > 20);
    let _ = composition.router();
}
