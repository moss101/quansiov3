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
    runtime_run_in(
        pool,
        TENANT,
        WORKSPACE,
        "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
    )
    .await
}

/// A running run in `tenant`'s `workspace` on the seeded `node`, bound to a new agent `thread`.
async fn runtime_run_in(
    pool: &sqlx::PgPool,
    tenant: &str,
    workspace: &str,
    node: &str,
    thread: &str,
) -> (quansio_core::CanonicalId, u64) {
    use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
    // A run binds an agent thread, so the fixture creates the thread it binds rather than naming one that
    // does not exist.
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind) \
         VALUES ($1, $2, $3, 'worker')",
    )
    .bind(thread)
    .bind(tenant)
    .bind(workspace)
    .execute(pool)
    .await
    .expect("seed an agent thread");
    use quansio_server::runtime::state_machine::{
        NewRun, RunStatus, RunTriggerKind, RuntimeIdentity, RuntimeStore,
    };
    let identity = RuntimeIdentity::system(
        tenant,
        "api-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let store = RuntimeStore::new(pool.clone(), identity).expect("runtime store");
    let run = store
        .create_run(NewRun::new(
            workspace.to_string(),
            CanonicalId::parse_typed(node, Prefix::WorkNode).expect("work node"),
            CanonicalId::parse_typed(thread, Prefix::AgentThread).expect("agent thread"),
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
async fn a_refused_command_is_recorded_and_a_resend_gets_the_same_refusal() {
    let Some((router, pool, name)) = service("api_refused").await else {
        common::blocked_marker();
        return;
    };
    let (run, generation) = runtime_run(&pool).await;
    let run_id = run.to_string();
    // The run's owner fails it, so a cancellation is no longer a legal edge (§5.2) and the runtime refuses.
    {
        use quansio_core::{CorrelationId, Generation, UlidGenerator};
        use quansio_server::runtime::state_machine::{RunStatus, RuntimeIdentity, RuntimeStore};
        let identity = RuntimeIdentity::system(
            TENANT,
            "api-test",
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        RuntimeStore::new(pool.clone(), identity)
            .expect("runtime store")
            .transition_run(
                &run,
                Generation::new(generation).expect("generation"),
                RunStatus::Failed,
                Some("fixture".to_string()),
            )
            .await
            .expect("fail the run");
    }
    let body = serde_json::json!({
        "command_id": COMMAND,
        "params": { "run_id": run_id, "generation": generation },
    });
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
    let status = response.status();
    let first = json_body(response).await;
    assert_eq!(status, StatusCode::CONFLICT, "{first}");
    assert_eq!(first["code"], "RUNTIME_ILLEGAL_TRANSITION", "{first}");

    // The refusal is the command's outcome (§1.2), so the record says so rather than staying accepted.
    let (record_status, error, result): (String, Option<Value>, Option<Value>) = sqlx::query_as(
        "SELECT status, error, result FROM commands WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(TENANT)
    .bind(COMMAND)
    .fetch_one(&pool)
    .await
    .expect("the command record");
    assert_eq!(record_status, "rejected");
    assert_eq!(error.as_ref(), Some(&first), "the record keeps the refusal");
    assert_eq!(result, None);

    // A resend is answered with the original refusal and its status, not with a 200 that reads as success.
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
    let status = response.status();
    let again = json_body(response).await;
    assert_eq!(status, StatusCode::CONFLICT, "{again}");
    assert_eq!(
        again, first,
        "a resend gets the original refusal, correlation id included"
    );

    // The refusal changed nothing: the run is still the one its owner failed.
    let run_status: String =
        sqlx::query_scalar("SELECT status FROM runs WHERE id = $1 AND tenant_id = $2")
            .bind(&run_id)
            .bind(TENANT)
            .fetch_one(&pool)
            .await
            .expect("status");
    assert_eq!(run_status, "FAILED");

    // The refused command_id still cannot be reused for different parameters.
    let mut changed = body.clone();
    changed["params"]["generation"] = serde_json::json!(generation + 1);
    let response = router
        .clone()
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

    // A refusal raised while reading the parameters is the outcome too. Generation 0 is below the first
    // generation there is, so no run can carry it.
    let invalid = serde_json::json!({
        "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0GGGGG",
        "params": { "run_id": run_id, "generation": 0 },
    });
    let mut answers = Vec::new();
    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/v1/commands/CancelRun",
                Some(TENANT),
                Some(invalid.clone()),
            ))
            .await
            .expect("a response");
        answers.push((response.status(), json_body(response).await));
    }
    assert_eq!(answers[0].0, StatusCode::BAD_REQUEST, "{}", answers[0].1);
    assert_eq!(
        answers[0].1["code"], "VALIDATION_BOUNDS",
        "{}",
        answers[0].1
    );
    assert_eq!(answers[1], answers[0], "the resend gets the same refusal");
    let record_status: String =
        sqlx::query_scalar("SELECT status FROM commands WHERE tenant_id = $1 AND command_id = $2")
            .bind(TENANT)
            .bind("cmd_01J8Z3K6F1N8VQ2X5W9Y0GGGGG")
            .fetch_one(&pool)
            .await
            .expect("the command record");
    assert_eq!(record_status, "rejected");
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_resend_while_the_first_is_still_being_applied_is_told_to_come_back() {
    let Some((router, pool, name)) = service("api_in_flight").await else {
        common::blocked_marker();
        return;
    };
    let (run, generation) = runtime_run(&pool).await;
    let params = serde_json::json!({ "run_id": run.to_string(), "generation": generation });
    // The first submission's record as it stands while its owner is still working: accepted, with no
    // outcome yet.
    sqlx::query(
        "INSERT INTO commands (id, tenant_id, command_name, command_id, params_digest, status) \
         VALUES ('cmd_01J8Z3K6F1N8VQ2X5W9Y0FFFFF', $1, 'CancelRun', $2, $3, 'accepted')",
    )
    .bind(TENANT)
    .bind(COMMAND)
    .bind(quansio_core::Digest::of_canonical_json(&params.to_string()).as_str())
    .execute(&pool)
    .await
    .expect("an accepted record");

    let response = router
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(TENANT),
            Some(serde_json::json!({ "command_id": COMMAND, "params": params })),
        ))
        .await
        .expect("a response");
    let status = response.status();
    let body = json_body(response).await;
    // Not a 200 with no result: the outcome is not known yet, and a resend later will get it.
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "CONFLICT_STATE", "{body}");
    assert_eq!(body["retryable"], true, "{body}");
    assert_eq!(body["details"]["command_status"], "accepted", "{body}");

    // The duplicate did not call the owner a second time.
    let run_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = $1")
        .bind(run.to_string())
        .fetch_one(&pool)
        .await
        .expect("status");
    assert_eq!(run_status, "RUNNING");
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
async fn two_tenants_may_send_the_same_command_id_and_each_replays_its_own() {
    let Some((router, pool, name)) = service("api_tenant_command_id").await else {
        common::blocked_marker();
        return;
    };
    // §1.2: a command_id is unique per tenant. Clients mint it, so two tenants can mint the same one, and
    // neither may break the other's retry or learn from the answer that the other already used it.
    const OTHER: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
    const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
    const OTHER_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
    common::seed_tenant(
        &pool,
        OTHER,
        "usr_01J8Z3K6F1N8VQ2X5W9Y0DDDDD",
        OTHER_WORKSPACE,
        OTHER_NODE,
    )
    .await;
    let mut calls = Vec::new();
    for (tenant, workspace, node, thread) in [
        (
            TENANT,
            WORKSPACE,
            "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
            "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        ),
        (
            OTHER,
            OTHER_WORKSPACE,
            OTHER_NODE,
            "ath_01J8Z3K6F1N8VQ2X5W9Y0DDDDD",
        ),
    ] {
        let (run, generation) = runtime_run_in(&pool, tenant, workspace, node, thread).await;
        let body = serde_json::json!({
            "command_id": COMMAND,
            "params": { "run_id": run.to_string(), "generation": generation },
        });
        calls.push((tenant, run.to_string(), body));
    }

    // Each tenant's first submission is applied as a new command in that tenant, not refused and not
    // answered with the other tenant's record.
    let mut firsts = Vec::new();
    for (tenant, run_id, body) in &calls {
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/v1/commands/CancelRun",
                Some(tenant),
                Some(body.clone()),
            ))
            .await
            .expect("a response");
        let status = response.status();
        let first = json_body(response).await;
        assert_eq!(status, StatusCode::OK, "{tenant}: {first}");
        assert_eq!(first["replayed"], false, "{tenant}: {first}");
        assert_eq!(first["result"]["run_id"].as_str(), Some(run_id.as_str()));
        assert_eq!(first["result"]["status"], "CANCELLED", "{tenant}: {first}");
        firsts.push(first);
    }

    // A resend in either tenant is answered from that tenant's own record.
    for ((tenant, _, body), first) in calls.iter().zip(&firsts) {
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/v1/commands/CancelRun",
                Some(tenant),
                Some(body.clone()),
            ))
            .await
            .expect("a response");
        assert_eq!(response.status(), StatusCode::OK, "{tenant}'s resend");
        let again = json_body(response).await;
        assert_eq!(again["replayed"], true, "{tenant}: {again}");
        assert_eq!(again["result"], first["result"], "{tenant}: {again}");
    }

    // A parameter mismatch is judged against the sender's own record: it is refused in that tenant and the
    // other tenant still replays unchanged.
    let mut changed = calls[1].2.clone();
    let generation = changed["params"]["generation"]
        .as_u64()
        .expect("a generation");
    changed["params"]["generation"] = serde_json::json!(generation + 1);
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(OTHER),
            Some(changed),
        ))
        .await
        .expect("a response");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(response).await["code"],
        "CONFLICT_IDEMPOTENCY_MISMATCH"
    );
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(TENANT),
            Some(calls[0].2.clone()),
        ))
        .await
        .expect("a response");
    let again = json_body(response).await;
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(again["result"], firsts[0]["result"], "{again}");

    // One record per tenant, each with an id the server minted (§1.1) rather than the client's command_id.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT tenant_id, id FROM commands WHERE command_id = $1 ORDER BY tenant_id",
    )
    .bind(COMMAND)
    .fetch_all(&pool)
    .await
    .expect("command records");
    let tenants: Vec<&str> = rows.iter().map(|(tenant, _)| tenant.as_str()).collect();
    assert_eq!(tenants, vec![TENANT, OTHER]);
    for (tenant, id) in &rows {
        assert!(
            id.starts_with("cmd_") && id != COMMAND,
            "{tenant}'s record id: {id}"
        );
    }

    // A record written before row ids were minted carries the client's command_id as its id. Another
    // tenant sending that command_id is still served: the old rows need no rewrite.
    let legacy = "cmd_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
    sqlx::query(
        "INSERT INTO commands (id, tenant_id, command_name, command_id, params_digest, status, result) \
         VALUES ($1, $2, 'CancelRun', $1, 'legacy', 'applied', '{}'::jsonb)",
    )
    .bind(legacy)
    .bind(TENANT)
    .execute(&pool)
    .await
    .expect("a record in the pre-fix shape");
    let (run, generation) = runtime_run_in(
        &pool,
        OTHER,
        OTHER_WORKSPACE,
        OTHER_NODE,
        "ath_01J8Z3K6F1N8VQ2X5W9Y0EEEEE",
    )
    .await;
    let response = router
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(OTHER),
            Some(serde_json::json!({
                "command_id": legacy,
                "params": { "run_id": run.to_string(), "generation": generation },
            })),
        ))
        .await
        .expect("a response");
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["replayed"], false, "{body}");
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

#[test]
fn feature_flags_load_from_config_and_cannot_disable_escalation() {
    let flags = quansio_server::api::FeatureFlags::load().expect("flags");
    assert!(
        flags.enabled("trust.escalation.enabled"),
        "escalation is authority, not a rollout switch that can go off"
    );
    assert!(flags.get("runtime.compaction.epochs").is_some());
}

#[test]
fn generated_openapi_lists_every_catalog_command() {
    let spec: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../../../schemas/openapi/public-api-v1.yaml"))
            .expect("openapi");
    let catalog = quansio_server::api::Catalog::from_embedded().expect("catalog");
    let paths = spec.get("paths").and_then(serde_yaml::Value::as_mapping);
    let paths = paths.expect("paths");
    for name in catalog.commands() {
        let path = format!("/v1/commands/{name}");
        assert!(
            paths.contains_key(serde_yaml::Value::String(path.clone())),
            "generated OpenAPI is missing {path}"
        );
    }
}

#[tokio::test]
async fn a_tenant_over_the_rate_limit_is_rate_limited() {
    let name = common::scratch_name("api_rate");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
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
    let state = quansio_server::api::ApiState::new(pool.clone())
        .expect("api")
        .with_limits(quansio_server::api::RateLimiter::new(
            1,
            std::time::Duration::from_secs(60),
        ));
    let router = quansio_server::api::router(state);
    let (run, generation) = runtime_run(&pool).await;
    let first = router
        .clone()
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(TENANT),
            Some(serde_json::json!({
                "command_id": COMMAND,
                "params": { "run_id": run.to_string(), "generation": generation }
            })),
        ))
        .await
        .expect("first");
    assert_eq!(first.status(), StatusCode::OK);
    let second = router
        .oneshot(request(
            "POST",
            "/v1/commands/CancelRun",
            Some(TENANT),
            Some(serde_json::json!({
                "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0DDDDD",
                "params": { "run_id": run.to_string(), "generation": generation }
            })),
        ))
        .await
        .expect("second");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_body(second).await["code"], "RATE_LIMITED");
    // The limit is checked before the command is recorded, so the command_id stays free and a resend
    // once the window has passed is a real retry rather than a replay of the refusal.
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM commands WHERE command_id = $1")
        .bind("cmd_01J8Z3K6F1N8VQ2X5W9Y0DDDDD")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(recorded, 0);
    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ the application role

#[test]
fn every_statement_the_api_runs_is_inside_a_tenant_scoped_transaction() {
    // Row-level security is forced on every tenant table, and the tenant setting lasts one transaction
    // (`set_config(.., true)`). A statement on the bare pool, or on a connection taken from it outside a
    // transaction, therefore runs with no tenant: as the application role it reads nothing and writes
    // nothing. The superuser the development stack connects as bypasses the policies, so no handler's
    // output shows this; the module is scanned instead, the way the ownership boundary above is.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    let executors = [
        "execute",
        "fetch",
        "fetch_one",
        "fetch_all",
        "fetch_optional",
    ];
    let unscoped = ["state.pool()", "pool", "&pool", "self.pool()", "&self.pool"];
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
            // Whitespace is dropped so a call split over lines reads as one.
            let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            let mut found = Vec::new();
            for executor in executors {
                for pool in unscoped {
                    for close in [")", ",)"] {
                        let call = format!(".{executor}({pool}{close}");
                        found.extend(flat.match_indices(&call).map(|(at, _)| at));
                    }
                }
            }
            found.extend(flat.match_indices(".acquire()").map(|(at, _)| at));
            for at in found {
                let statement = &flat[flat[..at].rfind(';').map_or(0, |end| end + 1)..at];
                // The readiness probe names no table: it asks whether the database answers at all.
                if statement.contains("(\"SELECT1\")") {
                    continue;
                }
                // The statement's tail is enough to find it: the query text and its executor.
                let tail: String = statement
                    .chars()
                    .rev()
                    .take(160)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                offenders.push(format!("{}: ...{tail}", path.display()));
            }
        }
    }
    assert!(scanned >= 2, "the scan must actually read the module");
    assert!(
        offenders.is_empty(),
        "the API runs a statement outside a tenant-scoped transaction: {offenders:#?}"
    );
}

/// A pool whose every connection acts as `quansio_app`, the least-privilege role a deployment connects as.
async fn app_role_pool(url: &str) -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET ROLE quansio_app").execute(conn).await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .expect("app-role pool")
}

fn as_user(method: &str, path: &str, body: Option<Value>) -> Request<Body> {
    let mut request = request(method, path, Some(TENANT), body);
    request
        .headers_mut()
        .insert("x-quansio-user", USER.parse().expect("header"));
    request
}

async fn call(router: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.expect("a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("a body");
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(format!("{} bytes", bytes.len())));
    (status, body)
}

#[tokio::test]
async fn the_public_surface_works_as_the_application_role_under_rls() {
    use base64::Engine;
    use quansio_server::artifacts::{
        CapturedBy, EvidenceContent, EvidenceKind, EvidenceStore, NewEvidence, ObjectStore,
        S3ObjectStore,
    };
    use serde_json::json;
    use std::sync::Arc;

    const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
    const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
    const LEGACY_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCD";
    const LEGACY_THREAD: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCD";
    let command = |n: u32| format!("cmd_01J8Z3K6F1N8VQ2X5W9Y0C{n:04}");

    let name = common::scratch_name("api_app_role");
    let Some(admin) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    // The fixture is written by the superuser, as provisioning would; everything after it goes through a
    // router whose pool is the application role, where every tenant table's policy is in force.
    quansio_server::control::schema::migrate(&admin)
        .await
        .expect("migrate");
    common::seed_tenant(&admin, TENANT, USER, WORKSPACE, NODE).await;
    {
        let mut tx = admin.begin().await.expect("begin");
        quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        sqlx::query(
            "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
             VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC', $1, NULL, 'tenant', $2)",
        )
        .bind(TENANT)
        .bind(json!([
            { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }
        ]))
        .execute(&mut *tx)
        .await
        .expect("policy");
        // The workspace's execution target, which a message's run binds when its teammate names none.
        sqlx::query(
            "INSERT INTO execution_targets (id, tenant_id, workspace_id, target_class, substrate, status) \
             VALUES ($1, $2, $3, 'persistent_workspace_computer', 'local_capsule_macos', 'READY')",
        )
        .bind(TARGET)
        .bind(TENANT)
        .bind(WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("target");
        // A second workspace that still has a pre-definition teammate thread, which its messages keep.
        sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'legacy')")
            .bind(LEGACY_WORKSPACE)
            .bind(TENANT)
            .execute(&mut *tx)
            .await
            .expect("workspace");
        sqlx::query(
            "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
             VALUES ('wm_01J8Z3K6F1N8VQ2X5W9Y0CCCCD', $1, $2, $3, 'admin', 'active')",
        )
        .bind(TENANT)
        .bind(LEGACY_WORKSPACE)
        .bind(USER)
        .execute(&mut *tx)
        .await
        .expect("membership");
        sqlx::query(
            "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) \
             VALUES ('wn_01J8Z3K6F1N8VQ2X5W9Y0CCCCD', $1, $2, 'objective', 'legacy node', \
                     '{\"kind\":\"user\",\"id\":\"usr_seed\"}'::jsonb)",
        )
        .bind(TENANT)
        .bind(LEGACY_WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("work node");
        sqlx::query(
            "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, status) \
             VALUES ($1, $2, $3, 'teammate', 'ACTIVE')",
        )
        .bind(LEGACY_THREAD)
        .bind(TENANT)
        .bind(LEGACY_WORKSPACE)
        .execute(&mut *tx)
        .await
        .expect("legacy teammate thread");
        tx.commit().await.expect("commit");
    }
    let (run, generation) = runtime_run(&admin).await;
    let objects: Arc<dyn ObjectStore> =
        Arc::new(S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration"));
    let evidence = {
        let mut conn = admin.acquire().await.expect("conn");
        EvidenceStore::new(objects.as_ref())
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
                        bytes: b"exit 0\n".to_vec(),
                        media_type: "text/plain".to_string(),
                    },
                    inline_summary: Some("exit 0".to_string()),
                    captured_at: None,
                    captured_by: CapturedBy {
                        kind: "run".to_string(),
                        id: run.to_string(),
                    },
                    redaction_applied: false,
                },
            )
            .await
            .expect("evidence")
            .id
            .to_canonical()
    };

    let pool = app_role_pool(&common::scratch_url(&name)).await;
    let state = ApiState::new(pool.clone())
        .expect("api state")
        .with_object_store(objects);
    let router = quansio_server::api::router(state);

    let (status, health) = call(&router, request("GET", "/v1/health", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{health}");

    // A message: recorded in the command log, routed to the workspace's default teammate, bound to the
    // workspace's execution target, projected, run to a question.
    let post = json!({
        "command_id": command(1),
        "params": {
            "workspace_id": WORKSPACE,
            "content": format!("pick for me {}", quansio_server::api::seams::QUESTION_SCENARIO),
        },
    });
    let (status, posted) = call(
        &router,
        as_user("POST", "/v1/commands/PostMessage", Some(post.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    assert_eq!(posted["replayed"], false, "{posted}");
    assert_eq!(posted["result"]["turn"]["kind"], "parked", "{posted}");
    let thread_id = posted["result"]["thread_id"]
        .as_str()
        .expect("thread")
        .to_string();
    let run_id = posted["result"]["run_id"]
        .as_str()
        .expect("run")
        .to_string();
    let bound: Option<String> =
        sqlx::query_scalar("SELECT execution_target_id FROM runs WHERE id = $1")
            .bind(&run_id)
            .fetch_one(&admin)
            .await
            .expect("the run");
    assert_eq!(bound.as_deref(), Some(TARGET), "the workspace's target");

    // A resend is answered from the command log.
    let (status, again) = call(
        &router,
        as_user("POST", "/v1/commands/PostMessage", Some(post)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(again["result"], posted["result"], "{again}");

    // The run's projection is readable, with the tenant policy it was resolved from.
    let (status, projected) =
        call(&router, as_user("GET", &format!("/v1/runs/{run_id}"), None)).await;
    assert_eq!(status, StatusCode::OK, "{projected}");
    let inputs = projected["capability_projection"]["inputs"]
        .as_array()
        .unwrap_or_else(|| panic!("the run has no capability projection: {projected}"));
    assert!(
        inputs
            .iter()
            .any(|input| input.to_string().contains("pol_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")),
        "the projection names the tenant policy: {projected}"
    );

    // Answering the question resumes the run, and the reply lands in the thread the run came from.
    let (status, questions) = call(
        &router,
        as_user("GET", &format!("/v1/questions?run_id={run_id}"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{questions}");
    let question_id = questions["questions"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no open question: {questions}"))
        .to_string();
    let (status, answered) = call(
        &router,
        as_user(
            "POST",
            "/v1/commands/AnswerQuestion",
            Some(json!({
                "command_id": command(2),
                "params": { "question_id": question_id, "value": "alpha" },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert_eq!(answered["result"]["thread_id"], thread_id, "{answered}");
    assert!(
        answered["result"]["reply_message_id"].is_string(),
        "{answered}"
    );
    let (status, messages) = call(
        &router,
        as_user("GET", &format!("/v1/messages?thread_id={thread_id}"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{messages}");
    assert_eq!(
        messages["messages"].as_array().map(Vec::len),
        Some(2),
        "{messages}"
    );

    // A workspace with a pre-definition teammate thread keeps it.
    let (status, legacy) = call(
        &router,
        as_user(
            "POST",
            "/v1/commands/PostMessage",
            Some(json!({
                "command_id": command(3),
                "params": { "workspace_id": LEGACY_WORKSPACE, "content": "hello" },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{legacy}");
    let thread: String = sqlx::query_scalar("SELECT agent_thread_id FROM runs WHERE id = $1")
        .bind(legacy["result"]["run_id"].as_str().expect("run"))
        .fetch_one(&admin)
        .await
        .expect("the run");
    assert_eq!(thread, LEGACY_THREAD, "{legacy}");

    // A run command through the runtime, and a refusal, each kept as the command's outcome.
    let (status, cancelled) = call(
        &router,
        as_user(
            "POST",
            "/v1/commands/CancelRun",
            Some(json!({
                "command_id": command(4),
                "params": { "run_id": run.to_string(), "generation": generation },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["result"]["status"], "CANCELLED", "{cancelled}");
    let refused_call = json!({
        "command_id": command(5),
        "params": { "run_id": run.to_string(), "generation": 0 },
    });
    let (status, refused) = call(
        &router,
        as_user("POST", "/v1/commands/CancelRun", Some(refused_call.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    let (status, refused_again) = call(
        &router,
        as_user("POST", "/v1/commands/CancelRun", Some(refused_call)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused_again}");
    assert_eq!(
        refused_again, refused,
        "the resend gets the recorded refusal"
    );

    // An upload, and its bytes read back.
    let bytes: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
    let (status, created) = call(
        &router,
        as_user(
            "POST",
            "/v1/commands/CreateArtifact",
            Some(json!({
                "command_id": command(6),
                "params": {
                    "workspace_id": WORKSPACE,
                    "title": "sample.bin",
                    "media_type": "application/octet-stream",
                    "bytes_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let artifact_id = created["result"]["artifact_id"].as_str().expect("artifact");
    let version_id = created["result"]["version_id"].as_str().expect("version");
    let (status, content) = call(
        &router,
        as_user(
            "GET",
            &format!("/v1/artifacts/{artifact_id}/versions/{version_id}/content"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{content}");
    assert_eq!(content, json!("1024 bytes"), "{content}");

    // Evidence, as metadata and as bytes.
    let (status, record) = call(
        &router,
        as_user("GET", &format!("/v1/evidence/{evidence}"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{record}");
    assert_eq!(record["id"], evidence, "{record}");
    let (status, evidence_bytes) = call(
        &router,
        as_user("GET", &format!("/v1/evidence/{evidence}/content"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{evidence_bytes}");

    // Every command was recorded with its outcome, in this tenant.
    let outcomes: Vec<(String, String)> = sqlx::query_as(
        "SELECT command_id, status FROM commands WHERE tenant_id = $1 ORDER BY command_id",
    )
    .bind(TENANT)
    .fetch_all(&admin)
    .await
    .expect("the command log");
    assert_eq!(
        outcomes,
        vec![
            (command(1), "applied".to_string()),
            (command(2), "applied".to_string()),
            (command(3), "applied".to_string()),
            (command(4), "applied".to_string()),
            (command(5), "rejected".to_string()),
            (command(6), "applied".to_string()),
        ]
    );
    drop(pool);
    common::drop_pool(&admin, &name).await;
}
