//! Tool visibility through the server's real dispatcher (ADP-008, DOMAIN.md §7.5), against a real PostgreSQL
//! database, the development MinIO and the real engine:
//!
//! * **a model that names a host-only tool is refused, and it is recorded**: the call row is `rejected` with
//!   `TOOL_NOT_MODEL_VISIBLE` and `caller: model`, a `tool.rejected` event carries the same, and no EffectRecord
//!   exists. It is refused before its arguments are read, so a guessed name with junk arguments gets the same answer;
//! * **the host reaches it through the same path**: the call is validated, authorized against the run's projection,
//!   evaluated by policy, reserved on the Effect Ledger, dispatched and settled, and the row says `caller: host`;
//! * **the host is not a bypass**: an empty projection refuses a host call (`CAPABILITY_DENIED`), a tool that is not
//!   `host_callable` is `TOOL_NOT_HOST_CALLABLE`, and both are recorded;
//! * **the model's tool list**: the tools a run's projection exposes hold the model's tools and not the host-only one,
//!   although the projection's grants cover it.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `QUANSIO_TEST_MINIO_*` (see `tests/artifacts.rs`).

use chrono::Utc;
use quansio_server::control::schema;
use quansio_tools::ToolRegistry;
use serde_json::json;

mod common;
use common::blocked_marker;
use common::outputs::{Harness, TENANT};

const HOST_ONLY: &str = "webdev.preview_scopes";

async fn harness(prefix: &str) -> Option<Harness> {
    let harness = Harness::prepare(prefix, |host| host).await;
    if harness.is_none() {
        blocked_marker();
    }
    harness
}

async fn count(h: &Harness, statement: &str, bind: &str) -> i64 {
    let mut tx = h.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let n: i64 = sqlx::query_scalar(statement)
        .bind(TENANT)
        .bind(bind)
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    n
}

#[tokio::test]
async fn a_model_naming_a_host_only_tool_is_refused_and_it_is_recorded() {
    let Some(h) = harness("adp008_model").await else {
        return;
    };
    // The projection the harness seeds covers read.internal over everything, so only visibility stands in the way.
    let declaration = ToolRegistry::builtin().get(HOST_ONLY).expect("declared");
    assert!(!declaration.model_visible && declaration.host_callable);

    let refused = h.call(HOST_ONLY, json!({})).await;
    assert_eq!(refused.status, "rejected", "{refused:#?}");
    assert_eq!(
        refused.code(),
        Some("TOOL_NOT_MODEL_VISIBLE"),
        "{refused:#?}"
    );
    assert_eq!(refused.caller, "model");
    let run = h.last_run();
    // A guess with junk arguments is the same answer: the arguments are not read.
    let guessed = h.call(HOST_ONLY, json!({ "bogus": 1 })).await;
    assert_eq!(
        guessed.code(),
        Some("TOOL_NOT_MODEL_VISIBLE"),
        "{guessed:#?}"
    );

    // Recorded as an event the operator can read.
    let events = count(
        &h,
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = 'tool.rejected' \
         AND payload ->> 'tool' = $2 AND payload ->> 'caller' = 'model' \
         AND payload ->> 'code' = 'TOOL_NOT_MODEL_VISIBLE'",
        HOST_ONLY,
    )
    .await;
    assert_eq!(events, 2, "both refusals are events");
    // ...and nothing was reserved, in either run.
    let effects = count(
        &h,
        "SELECT count(*) FROM effect_records WHERE tenant_id = $1 AND run_id = $2",
        &run,
    )
    .await;
    assert_eq!(effects, 0, "a refused proposal reserves no effect");
    h.finish().await;
}

#[tokio::test]
async fn the_host_reaches_a_host_only_tool_through_the_same_policy_and_ledger_path() {
    let Some(h) = harness("adp008_host").await else {
        return;
    };
    // No preview server is composed in this harness, so the tool host answers `ADAPTER_UNAVAILABLE`: the call
    // still ran the whole governed path, which is what is under test here (the success path, with a preview server
    // composed, is in `outputs_preview.rs`).
    let called = h.host_call(HOST_ONLY, json!({})).await;
    assert_eq!(called.caller, "host", "{called:#?}");
    assert_ne!(
        called.code(),
        Some("TOOL_NOT_MODEL_VISIBLE"),
        "a host call is not refused for being host-only: {called:#?}"
    );
    assert_ne!(called.status, "rejected", "{called:#?}");
    let run = h.last_run();
    let effects = count(
        &h,
        "SELECT count(*) FROM effect_records WHERE tenant_id = $1 AND run_id = $2 \
         AND effect_class = 'read.internal' AND status LIKE 'SETTLED%'",
        &run,
    )
    .await;
    assert_eq!(effects, 1, "the host call reserved and settled one effect");
    let decisions = count(
        &h,
        "SELECT count(*) FROM policy_decisions WHERE tenant_id = $1 AND run_id = $2",
        &run,
    )
    .await;
    assert!(decisions >= 1, "policy decided the host call");
    h.finish().await;
}

#[tokio::test]
async fn the_host_is_still_subject_to_the_capability_projection() {
    let Some(h) = harness("adp008_cap").await else {
        return;
    };
    let denied = h.host_call_with(HOST_ONLY, json!({}), &[]).await;
    assert_eq!(denied.status, "rejected", "{denied:#?}");
    assert_eq!(denied.caller, "host");
    assert!(
        matches!(
            denied.code(),
            Some("CAPABILITY_DENIED" | "CAPABILITY_INPUTS_UNAVAILABLE")
        ),
        "{denied:#?}"
    );
    let effects = count(
        &h,
        "SELECT count(*) FROM effect_records WHERE tenant_id = $1 AND run_id = $2",
        &h.last_run(),
    )
    .await;
    assert_eq!(effects, 0, "a refused host call reserves no effect");
    h.finish().await;
}

#[tokio::test]
async fn the_host_may_not_call_a_tool_that_is_not_host_callable() {
    let Some(h) = harness("adp008_notcallable").await else {
        return;
    };
    let refused = h.host_call("fs.read", json!({ "path": "a.txt" })).await;
    assert_eq!(refused.status, "rejected", "{refused:#?}");
    assert_eq!(
        refused.code(),
        Some("TOOL_NOT_HOST_CALLABLE"),
        "{refused:#?}"
    );
    assert_eq!(refused.caller, "host");
    let effects = count(
        &h,
        "SELECT count(*) FROM effect_records WHERE tenant_id = $1 AND run_id = $2",
        &h.last_run(),
    )
    .await;
    assert_eq!(effects, 0);
    h.finish().await;
}

#[tokio::test]
async fn the_tools_a_run_exposes_to_its_model_exclude_a_host_only_tool_the_grants_cover() {
    let Some(h) = harness("adp008_list").await else {
        return;
    };
    // Make a run with a projection (the harness seeds one with read.internal over everything).
    let _ = h.call(HOST_ONLY, json!({})).await;
    let run = h.last_run();
    let exposed = h
        .dispatcher
        .exposed_tools(&run, Utc::now())
        .await
        .expect("exposed tools");
    assert!(
        exposed.iter().any(|name| name == "webdev.preview_open"),
        "the model's own read tools are exposed: {exposed:?}"
    );
    assert!(
        !exposed.iter().any(|name| name == HOST_ONLY),
        "the host-only tool is not: {exposed:?}"
    );
    let declaration = ToolRegistry::builtin().get(HOST_ONLY).expect("declared");
    let grants = h
        .dispatcher
        .registry()
        .declarations()
        .find(|declaration| declaration.name == "webdev.preview_open")
        .expect("preview_open")
        .grant_templates
        .clone();
    assert!(
        declaration.covered_by(&grants, Utc::now()),
        "the grants that expose preview_open do cover the host-only tool: only visibility excludes it"
    );
    h.finish().await;
}
