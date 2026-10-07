//! Pack installs by content-hash revision, retention, rollback and install reports (ADP-008, DOMAIN.md 11.6), against a
//! real PostgreSQL database, the real intelligence plane (the Python process, over gRPC) and the real API router.
//!
//! * **revision**: installing the same content again is idempotent (the same revision, nothing moves); a changed
//!   file or field is a new revision; a digest that does not name the content, or a revision the caller named wrongly,
//!   is refused;
//! * **a failed install leaves the previous revision active**, at every phase: `fetch` (the source cannot be read),
//!   `verify` (a digest that is not the content's), `compile` (a pack naming a host-only tool; no plane), `activate`
//!   (the revision active is not the one the caller expected, found by a concurrent install). The report says
//!   `retained_previous`, and no new revision row exists;
//! * **recovery**: an install stopped between phases (the future is aborted, as a crashed server would be) leaves
//!   the previous revision active and a `running` report that the next read closes as interrupted; the same command
//!   id replays the failure, a new one installs;
//! * **rollback** is deterministic, bounded by `retain_previous_revisions`, and goes forward again;
//! * **reports** are redacted wherever the text comes from (a path, a URL, the plane's own words), bounded, expiring and
//!   tenant-scoped under row-level security, and readable through `GET /v1/packs`.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, and `uv` for the plane (without it the tests print `BLOCKED_EXTERNAL`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::Request;
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::Actor;
use quansio_server::api::ApiState;
use quansio_server::control::packs::{
    Compilation, CompileRequest, CompileUnavailable, Diagnostic, GatewayPackCompiler, InstallError,
    InstallRequest, PackCompilePort, PackConfig, PackContent, PackInstaller, PackSource, PackStore,
    Phase, PhaseHook, Severity,
};
use quansio_server::control::schema;
use quansio_server::intelligence::IntelligenceClient;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::Notify;
use tower::ServiceExt;

mod common;
use common::intelligence::{start_with_provider, Service};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP008";
const OTHER_TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP009";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP008";
const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP009";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP008";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP009";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP008";
const OTHER_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP009";
const CANARY: &str = "qncy_test_canary_not_for_prod";
const HOST_ONLY: &str = "webdev.preview_scopes";

fn command(n: u32) -> String {
    format!("cmd_01J8Z3K6F1N8VQ2X5W9YP{n:05}")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn builtin_root() -> PathBuf {
    repo().join("packs/capabilities")
}

fn coding() -> Value {
    serde_yaml::from_str(
        &std::fs::read_to_string(builtin_root().join("coding/pack.yaml")).expect("coding pack"),
    )
    .expect("yaml")
}

/// Re-record `provenance.digest`, as a reviewer would after an edit.
fn recorded(mut manifest: Value) -> Value {
    let digest = PackContent {
        manifest: manifest.clone(),
        files: BTreeMap::new(),
    }
    .manifest_digest()
    .expect("digest");
    manifest["provenance"]["digest"] = json!(digest);
    manifest
}

fn edited(edit: impl FnOnce(&mut Value)) -> Value {
    let mut manifest = coding();
    edit(&mut manifest);
    recorded(manifest)
}

fn inline(manifest: Value) -> PackSource {
    PackSource::Inline(PackContent {
        manifest,
        files: BTreeMap::new(),
    })
}

fn revision_of(manifest: &Value) -> String {
    PackContent {
        manifest: manifest.clone(),
        files: BTreeMap::new(),
    }
    .revision()
    .expect("revision")
}

fn request(n: u32, source: PackSource) -> InstallRequest {
    InstallRequest {
        request_id: command(n),
        workspace_id: WORKSPACE.to_string(),
        source,
        expected_revision: None,
        expected_active: None,
        actor: Actor::user(USER),
        correlation_id: CorrelationId::generate(&mut UlidGenerator::new()),
    }
}

/// Stop the plane: `uv run` is only the launcher, so killing it leaves the Python server listening. Kill what
/// listens on the port.
fn stop_plane(plane: Service) {
    let port = plane
        .endpoint
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_string();
    drop(plane);
    if let Ok(listening) = std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .output()
    {
        for pid in String::from_utf8_lossy(&listening.stdout).split_whitespace() {
            let _ = std::process::Command::new("kill")
                .args(["-9", pid])
                .status();
        }
    }
}

struct Fixture {
    name: String,
    pool: PgPool,
    plane: Option<Service>,
    client: IntelligenceClient,
    config: PackConfig,
    store: PackStore,
    installer: PackInstaller,
}

async fn fixture(prefix: &str, tweak: impl FnOnce(&mut PackConfig)) -> Option<Fixture> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, USER, WORKSPACE, NODE).await;
    common::seed_tenant(&pool, OTHER_TENANT, OTHER_USER, OTHER_WORKSPACE, OTHER_NODE).await;
    // The plane's compile does not call a model, so its provider is never reached.
    let plane = start_with_provider(Some("http://127.0.0.1:1")).await?;
    let client = IntelligenceClient::connect(&plane.endpoint)
        .await
        .expect("connect to the plane");
    let mut config = PackConfig::shipped();
    tweak(&mut config);
    let store = PackStore::new(pool.clone(), Arc::new(config.clone()));
    let installer = PackInstaller::new(
        store.clone(),
        Some(Arc::new(GatewayPackCompiler::new(
            client.clone(),
            Duration::from_secs(30),
        ))),
    )
    .with_builtin_root(builtin_root());
    Some(Fixture {
        name,
        pool,
        plane: Some(plane),
        client,
        config,
        store,
        installer,
    })
}

impl Fixture {
    async fn finish(self) {
        if let Some(plane) = self.plane {
            stop_plane(plane);
        }
        common::drop_pool(&self.pool, &self.name).await;
    }

    async fn active(&self, name: &str) -> Option<String> {
        self.store
            .packs(TENANT)
            .await
            .expect("packs")
            .into_iter()
            .find(|pack| pack.name == name)
            .and_then(|pack| pack.active)
            .and_then(|active| active.revision)
    }

    async fn rows(&self, statement: &str) -> i64 {
        let mut tx = self.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let n: i64 = sqlx::query_scalar(statement)
            .fetch_one(&mut *tx)
            .await
            .expect("count");
        tx.commit().await.expect("commit");
        n
    }

    async fn installed(
        &self,
        n: u32,
        manifest: Value,
    ) -> quansio_server::control::packs::Installed {
        self.installer
            .install(TENANT, request(n, inline(manifest)))
            .await
            .unwrap_or_else(|error| panic!("install {n}: {error:?}"))
    }

    async fn failed(
        &self,
        n: u32,
        source: PackSource,
    ) -> quansio_server::control::packs::InstallReport {
        match self.installer.install(TENANT, request(n, source)).await {
            Err(InstallError::Failed(report)) => *report,
            other => panic!("install {n} should have failed: {other:?}"),
        }
    }
}

fn rules(report: &quansio_server::control::packs::InstallReport) -> Vec<&str> {
    report.diagnostics.iter().map(|d| d.rule.as_str()).collect()
}

// ---- revision and idempotency -----------------------------------------------------------------

#[tokio::test]
async fn a_pack_is_installed_by_its_content_hash_and_installing_it_again_is_idempotent() {
    let Some(f) = fixture("adp008_rev", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let v1 = coding();
    let revision = revision_of(&v1);
    let first = f.installed(1, v1.clone()).await;
    assert_eq!(first.report.revision.as_deref(), Some(revision.as_str()));
    assert_eq!(first.report.state.as_str(), "succeeded");
    assert_eq!(first.report.phase, Phase::Done);
    assert_eq!(first.report.outcome.map(|o| o.as_str()), Some("activated"));
    assert!(!first.unchanged && first.report.previous_revision.is_none());
    assert_eq!(f.active("coding").await.as_deref(), Some(revision.as_str()));

    // The same content under a new command: the same revision, nothing moves, and it is still `activated`.
    let again = f.installed(2, v1.clone()).await;
    assert!(again.unchanged);
    assert_eq!(again.report.revision.as_deref(), Some(revision.as_str()));
    assert_eq!(again.version_id, first.version_id);
    assert_eq!(
        f.rows("SELECT count(*) FROM capability_pack_versions")
            .await,
        1
    );

    // A re-review changes the record, not the revision.
    let mut reviewed = v1.clone();
    reviewed["review"]["reviewed_at"] = json!("2031-01-01T00:00:00Z");
    assert_eq!(
        f.installed(3, reviewed).await.report.revision,
        again.report.revision
    );

    // A change to what executes is a new revision, and the old one is retained, not deleted.
    let v2 = edited(|m| m["semver"] = json!("1.1.0"));
    let second = f.installed(4, v2.clone()).await;
    assert_ne!(second.report.revision, first.report.revision);
    assert_eq!(
        second.report.previous_revision.as_deref(),
        Some(revision.as_str())
    );
    let summary = f.store.packs(TENANT).await.expect("packs");
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].revisions.len(), 1);
    assert_eq!(summary[0].revisions[0].status, "deprecated");
    assert!(summary[0].revisions[0].has_content);

    // The command id replays what it left.
    let replay = f
        .installer
        .install(TENANT, request(4, inline(v2)))
        .await
        .expect("replay");
    assert!(replay.replayed && replay.report.revision == second.report.revision);

    // A built-in pack installs from the repository's own files, and its recorded digest names its content.
    let research = f
        .installer
        .install(
            TENANT,
            request(5, PackSource::Builtin("research".to_string())),
        )
        .await
        .expect("research");
    assert_eq!(research.report.pack_name, "research");
    assert_eq!(f.active("research").await, research.report.revision);
    f.finish().await;
}

#[tokio::test]
async fn a_revision_that_is_not_the_content_and_a_digest_that_does_not_name_it_are_refused() {
    let Some(f) = fixture("adp008_digest", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let v1 = coding();
    f.installed(1, v1.clone()).await;
    let active = f.active("coding").await;

    // The caller names a revision that is not the content's.
    let mut wrong = request(2, inline(edited(|m| m["semver"] = json!("2.0.0"))));
    wrong.expected_revision = Some(format!("sha256:{}", "0".repeat(64)));
    let report = match f.installer.install(TENANT, wrong).await {
        Err(InstallError::Failed(report)) => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.phase, Phase::Verify);
    assert!(
        rules(&report).contains(&"pack.revision"),
        "{:?}",
        rules(&report)
    );
    assert_eq!(
        report.outcome.map(|o| o.as_str()),
        Some("retained_previous")
    );

    // The content was edited after it was reviewed: the recorded digest no longer names it.
    let mut stale = coding();
    stale["contents"]["examples"] = json!(["something nobody reviewed"]);
    let report = f.failed(3, inline(stale)).await;
    assert_eq!(report.phase, Phase::Verify);
    assert!(
        rules(&report).contains(&"pack.digest"),
        "{:?}",
        rules(&report)
    );
    assert_eq!(f.active("coding").await, active);
    assert_eq!(
        f.rows("SELECT count(*) FROM capability_pack_versions")
            .await,
        1
    );
    f.finish().await;
}

// ---- a failed install leaves the previous revision active ---------------------------------------

#[tokio::test]
async fn a_failed_install_leaves_the_previous_revision_active_at_every_phase() {
    let Some(f) = fixture("adp008_fail", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    f.installed(1, coding()).await;
    let active = f.active("coding").await.expect("active");
    let versions = f
        .rows("SELECT count(*) FROM capability_pack_versions")
        .await;

    // fetch: the source cannot be read (a built-in root that does not exist, for a pack of the same name).
    let missing = f
        .installer
        .clone()
        .with_builtin_root(repo().join("packs/nowhere"));
    let report = match missing
        .install(
            TENANT,
            request(2, PackSource::Builtin("coding".to_string())),
        )
        .await
    {
        Err(InstallError::Failed(report)) => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.phase, Phase::Fetch);
    assert_eq!(
        report.outcome.map(|o| o.as_str()),
        Some("retained_previous")
    );
    assert!(rules(&report).contains(&"pack.fetch.source"));

    // verify: a quarantine record that is not cleared (re-recorded, so only the quarantine rule fires).
    let report = f
        .failed(
            3,
            inline(edited(|m| m["quarantine"]["status"] = json!("pending"))),
        )
        .await;
    assert_eq!(report.phase, Phase::Verify);
    assert!(rules(&report).contains(&"pack.quarantine"));

    // verify: credential-shaped content in a file.
    let mut with_secret = PackContent {
        manifest: coding(),
        files: BTreeMap::new(),
    };
    with_secret.files.insert(
        "notes/howto.md".to_string(),
        format!("api_key: live-{CANARY}"),
    );
    let report = f.failed(4, PackSource::Inline(with_secret)).await;
    assert_eq!(report.phase, Phase::Verify);
    assert!(rules(&report).contains(&"pack.credential"));

    // compile: the pack names a tool no model may see.
    let report = f
        .failed(
            5,
            inline(edited(|m| {
                m["contents"]["tool_names"] = json!(["fs.read", HOST_ONLY, "fs.teleport"]);
            })),
        )
        .await;
    assert_eq!(report.phase, Phase::Compile);
    let found = rules(&report);
    assert!(
        found.contains(&"pack.tool.host_only") && found.contains(&"pack.tool.unknown"),
        "{found:?}"
    );
    assert_eq!(
        report.outcome.map(|o| o.as_str()),
        Some("retained_previous")
    );

    // None of them changed what is active or left a revision behind.
    assert_eq!(f.active("coding").await.as_deref(), Some(active.as_str()));
    assert_eq!(
        f.rows("SELECT count(*) FROM capability_pack_versions")
            .await,
        versions
    );
    // ...and each left a report: four failures and the success.
    let reports = f
        .store
        .reports(TENANT, Some("coding"), 50)
        .await
        .expect("reports");
    assert_eq!(
        reports.len(),
        5,
        "{:?}",
        reports.iter().map(|r| &r.request_id).collect::<Vec<_>>()
    );
    assert_eq!(
        reports
            .iter()
            .filter(|r| r.state.as_str() == "failed")
            .count(),
        4
    );
    f.finish().await;
}

#[tokio::test]
async fn a_first_install_that_fails_leaves_nothing_active_and_says_failed() {
    let Some(f) = fixture("adp008_first", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let report = f
        .failed(
            1,
            inline(edited(|m| m["contents"]["tool_names"] = json!([HOST_ONLY]))),
        )
        .await;
    assert_eq!(report.outcome.map(|o| o.as_str()), Some("failed"));
    assert!(f.store.packs(TENANT).await.expect("packs").is_empty());
    f.finish().await;
}

#[tokio::test]
async fn without_the_plane_nothing_installs_and_the_previous_revision_stays() {
    let Some(mut f) = fixture("adp008_noplane", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    f.installed(1, coding()).await;
    let active = f.active("coding").await;
    // A pack compiled by no one is not installed: no composed plane...
    let bare = PackInstaller::new(f.store.clone(), None);
    let report = match bare
        .install(
            TENANT,
            request(2, inline(edited(|m| m["semver"] = json!("1.5.0")))),
        )
        .await
    {
        Err(InstallError::Failed(report)) => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.phase, Phase::Compile);
    assert!(rules(&report).contains(&"pack.compile.unavailable"));
    // ...and the real plane process killed under the installer.
    stop_plane(f.plane.take().expect("the plane"));
    let report = f
        .failed(3, inline(edited(|m| m["semver"] = json!("1.6.0"))))
        .await;
    assert_eq!(report.phase, Phase::Compile);
    assert!(rules(&report).contains(&"pack.compile.unavailable"));
    assert_eq!(f.active("coding").await, active);
    let _ = &f.client;
    f.finish().await;
}

// ---- concurrency and recovery -------------------------------------------------------------------

/// A hook that holds one install between two phases until the test lets it go (or never does).
fn gate(at: Phase, n: u32) -> (PhaseHook, Arc<Notify>, Arc<Notify>) {
    let reached = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let (signal, wait, wanted) = (Arc::clone(&reached), Arc::clone(&proceed), command(n));
    let hook: PhaseHook = Arc::new(move |phase, request_id| {
        let (signal, wait, wanted) = (Arc::clone(&signal), Arc::clone(&wait), wanted.clone());
        Box::pin(async move {
            if phase == at && request_id == wanted {
                signal.notify_one();
                wait.notified().await;
            }
        })
    });
    (hook, reached, proceed)
}

#[tokio::test]
async fn an_install_that_finds_the_pack_changed_underneath_it_fails_at_activate_and_keeps_the_other(
) {
    let Some(f) = fixture("adp008_race", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    // A thinks nothing is installed yet and holds between compile and activate; B installs meanwhile.
    let (observer, reached, proceed) = gate(Phase::Compile, 1);
    let slow = f.installer.clone().with_phase_hook(observer);
    let mut a = request(1, inline(edited(|m| m["semver"] = json!("1.1.0"))));
    a.expected_active = Some(None);
    let handle = tokio::spawn(async move { slow.install(TENANT, a).await });
    reached.notified().await;
    let b = f.installed(2, coding()).await;
    proceed.notify_one();
    let report = match handle.await.expect("joined") {
        Err(InstallError::Failed(report)) => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.phase, Phase::Activate);
    assert!(
        rules(&report).contains(&"pack.activate.conflict"),
        "{:?}",
        rules(&report)
    );
    assert_eq!(
        report.outcome.map(|o| o.as_str()),
        Some("retained_previous")
    );
    assert_eq!(
        f.active("coding").await,
        b.report.revision,
        "B's revision is still the active one"
    );
    assert_eq!(
        f.rows("SELECT count(*) FROM capability_pack_versions")
            .await,
        1
    );
    f.finish().await;
}

#[tokio::test]
async fn an_install_stopped_between_phases_leaves_the_previous_revision_and_a_report_the_next_read_closes(
) {
    let Some(f) = fixture("adp008_crash", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    f.installed(1, coding()).await;
    let active = f.active("coding").await;
    let v2 = edited(|m| m["semver"] = json!("1.1.0"));

    // The server stops between compile and activate: the install's future is dropped where it stands.
    let (observer, reached, _never) = gate(Phase::Compile, 2);
    let stopped = f.installer.clone().with_phase_hook(observer);
    let source = inline(v2.clone());
    let handle = tokio::spawn(async move { stopped.install(TENANT, request(2, source)).await });
    reached.notified().await;
    handle.abort();
    assert!(handle.await.expect_err("aborted").is_cancelled());

    // Nothing moved; the report says where it was.
    assert_eq!(f.active("coding").await, active);
    assert_eq!(
        f.rows("SELECT count(*) FROM capability_pack_versions")
            .await,
        1
    );
    let left = f
        .store
        .report(TENANT, &command(2))
        .await
        .expect("read")
        .expect("a report");
    assert_eq!(left.state.as_str(), "running");
    assert_eq!(left.phase, Phase::Compile);
    assert!(left.outcome.is_none() && left.finished_at.is_none());

    // The restarted server (grace elapsed) closes it on the next read, with the previous revision retained.
    let mut restarted = f.config.clone();
    restarted.install.running_grace_seconds = 0;
    let store = PackStore::new(f.pool.clone(), Arc::new(restarted));
    let reports = store
        .reports(TENANT, Some("coding"), 50)
        .await
        .expect("reports");
    let closed = reports
        .iter()
        .find(|r| r.request_id == command(2))
        .expect("closed");
    assert_eq!(closed.state.as_str(), "failed");
    assert_eq!(closed.phase, Phase::Compile, "it stopped where it was");
    assert_eq!(
        closed.outcome.map(|o| o.as_str()),
        Some("retained_previous")
    );
    assert!(
        rules(closed).contains(&"pack.install.interrupted"),
        "{:?}",
        rules(closed)
    );
    assert_eq!(f.active("coding").await, active);

    // The same command id answers with the failure; a new one installs the same content.
    let replay = match f
        .installer
        .install(TENANT, request(2, inline(v2.clone())))
        .await
    {
        Err(InstallError::Failed(report)) => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(replay.request_id, command(2));
    assert_eq!(
        replay.outcome.map(|o| o.as_str()),
        Some("retained_previous")
    );
    let retried = f.installed(3, v2).await;
    assert_eq!(retried.report.previous_revision, active);
    assert_ne!(f.active("coding").await, active);
    f.finish().await;
}

// ---- retention and rollback ---------------------------------------------------------------------

fn version(n: u32) -> Value {
    edited(|m| m["semver"] = json!(format!("1.{n}.0")))
}

#[tokio::test]
async fn rollback_activates_the_previous_revision_deterministically_and_goes_forward_again() {
    let Some(f) = fixture("adp008_roll", |config| {
        config.install.retain_previous_revisions = 2
    })
    .await
    else {
        common::blocked_marker();
        return;
    };
    let (r1, r2, r3) = (
        revision_of(&version(1)),
        revision_of(&version(2)),
        revision_of(&version(3)),
    );
    for (n, v) in [(1, 1), (2, 2), (3, 3)] {
        f.installed(n, version(v)).await;
    }
    assert_eq!(f.active("coding").await.as_deref(), Some(r3.as_str()));
    let who = Actor::user(USER);
    let correlation = CorrelationId::generate(&mut UlidGenerator::new());

    let back = f
        .store
        .rollback(TENANT, "coding", None, &who, correlation)
        .await
        .expect("rollback");
    assert_eq!(
        (back.from_revision.as_deref(), back.to_revision.as_str()),
        (Some(r3.as_str()), r2.as_str())
    );
    assert_eq!(f.active("coding").await.as_deref(), Some(r2.as_str()));
    // The next rollback goes to the revision before that, not back to where it came from...
    let again = f
        .store
        .rollback(TENANT, "coding", None, &who, correlation)
        .await
        .expect("again");
    assert_eq!(
        again.to_revision, r3,
        "the most recently retired revision is r3 (it was retired last)"
    );
    // ...which is the deterministic rule: the most recently retired revision with content.
    assert_eq!(f.active("coding").await.as_deref(), Some(r3.as_str()));
    let events: Vec<String> = {
        let mut tx = f.pool.begin().await.expect("begin");
        schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let rows = sqlx::query_scalar(
            "SELECT type FROM runtime_events WHERE tenant_id = $1 AND aggregate_type = 'pack' ORDER BY sequence",
        )
        .bind(TENANT)
        .fetch_all(&mut *tx)
        .await
        .expect("events");
        tx.commit().await.expect("commit");
        rows
    };
    assert_eq!(
        events,
        [
            "pack.installed",
            "pack.installed",
            "pack.installed",
            "pack.rolled_back",
            "pack.rolled_back"
        ]
    );
    // A stale expectation is a conflict and moves nothing.
    let conflict = f
        .store
        .rollback(TENANT, "coding", Some(Some(r1.as_str())), &who, correlation)
        .await
        .expect_err("stale");
    assert_eq!(conflict.code(), "CONFLICT_REVISION");
    assert_eq!(f.active("coding").await.as_deref(), Some(r3.as_str()));
    // Unknown packs are not found.
    assert_eq!(
        f.store
            .rollback(TENANT, "nothing", None, &who, correlation)
            .await
            .expect_err("none")
            .code(),
        "NOT_FOUND"
    );
    f.finish().await;
}

#[tokio::test]
async fn retention_is_bounded_and_an_evicted_revision_keeps_its_row_and_loses_its_content() {
    let Some(f) = fixture("adp008_retain", |config| {
        config.install.retain_previous_revisions = 1
    })
    .await
    else {
        common::blocked_marker();
        return;
    };
    let r1 = revision_of(&version(1));
    for n in 1..=3 {
        f.installed(n, version(n)).await;
    }
    let pack = f.store.packs(TENANT).await.expect("packs").remove(0);
    let statuses: Vec<(&str, bool)> = pack
        .revisions
        .iter()
        .map(|r| (r.status.as_str(), r.has_content))
        .collect();
    assert_eq!(
        statuses,
        vec![("deprecated", true), ("withdrawn", false)],
        "one retained with content, the oldest kept as history without it"
    );
    let who = Actor::user(USER);
    let correlation = CorrelationId::generate(&mut UlidGenerator::new());
    // Rollback goes to the retained one; a withdrawn revision is never an answer.
    f.store
        .rollback(TENANT, "coding", None, &who, correlation)
        .await
        .expect("to v2");
    f.store
        .rollback(TENANT, "coding", None, &who, correlation)
        .await
        .expect("forward");
    assert_ne!(f.active("coding").await.as_deref(), Some(r1.as_str()));
    // A revision whose content was dropped can be installed again by its content: the row is revived.
    let revived = f.installed(4, version(1)).await;
    assert_eq!(revived.report.revision.as_deref(), Some(r1.as_str()));
    assert_eq!(f.active("coding").await.as_deref(), Some(r1.as_str()));
    assert_eq!(
        f.rows("SELECT count(*) FROM capability_pack_versions")
            .await,
        3,
        "a revision is one row, however often it is installed"
    );
    // Only one revision: nothing to roll back to.
    let solo = fixture("adp008_solo", |_| {}).await;
    if let Some(solo) = solo {
        solo.installed(1, coding()).await;
        let error = solo
            .store
            .rollback(TENANT, "coding", None, &who, correlation)
            .await
            .expect_err("nothing before");
        assert_eq!(error.code(), "CONFLICT_STATE");
        solo.finish().await;
    }
    f.finish().await;
}

// ---- reports: redaction, bounds, retention, isolation ------------------------------------------

/// A plane stand-in whose findings quote everything a finding must never keep. The real plane's wording is
/// fixed; this proves the runtime's own boundary no matter what a plane writes.
struct HostilePlane;

#[async_trait]
impl PackCompilePort for HostilePlane {
    async fn compile(
        &self,
        _request: CompileRequest<'_>,
    ) -> Result<Compilation, CompileUnavailable> {
        let message = format!(
            "fetched https://svc:{CANARY}@packs.example.test/p.tgz?token=zz9top&x=1 at /Users/jane.doe/work/p.yaml; \
             password: hunter2; Bearer abcdefghijklmnopqrstuvwx; key AKIAIOSFODNN7EXAMPLE; {}",
            "x".repeat(2_000)
        );
        let mut diagnostics =
            vec![
                Diagnostic::error(Phase::Compile, "pack.hostile", message.clone())
                    .at(format!("/files/{CANARY}/Users/jane.doe")),
            ];
        for n in 0..40 {
            diagnostics.push(Diagnostic::warning(
                Phase::Compile,
                "pack.noise",
                format!("noise {n}"),
            ));
        }
        Ok(Compilation {
            ok: false,
            failed_phase: Some(Phase::Compile),
            revision: String::new(),
            manifest_digest: String::new(),
            diagnostics,
            resolution: Value::Null,
        })
    }
}

const LEAKS: [&str; 9] = [
    CANARY,
    "svc:",
    "zz9top",
    "jane.doe",
    "hunter2",
    "abcdefghijklmnopqrstuvwx",
    "AKIAIOSFODNN7EXAMPLE",
    "deploy:",
    "abc123token",
];

async fn stored_text(f: &Fixture) -> String {
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT row_to_json(r)::text FROM pack_install_reports r")
            .fetch_all(&mut *tx)
            .await
            .expect("rows");
    tx.commit().await.expect("commit");
    rows.join("\n")
}

#[tokio::test]
async fn a_diagnostic_never_keeps_a_secret_a_home_directory_or_a_url_credential_and_is_bounded() {
    let Some(f) = fixture("adp008_redact", |config| {
        config.reports.max_diagnostics = 5;
        config.reports.max_diagnostic_bytes = 200;
    })
    .await
    else {
        common::blocked_marker();
        return;
    };
    f.installed(1, coding()).await;
    // 1. Words the plane wrote (a stand-in that quotes everything).
    let hostile = PackInstaller::new(f.store.clone(), Some(Arc::new(HostilePlane)))
        .with_builtin_root(builtin_root());
    let report = match hostile
        .install(
            TENANT,
            request(2, inline(edited(|m| m["semver"] = json!("3.0.0")))),
        )
        .await
    {
        Err(InstallError::Failed(report)) => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.diagnostics.len(), 5, "bounded in count");
    assert_eq!(
        report.diagnostics_omitted, 36,
        "and the count left out is kept"
    );
    assert_eq!(
        report.diagnostics[0].severity,
        Severity::Error,
        "errors first"
    );
    assert!(
        report.diagnostics.iter().all(|d| d.message.len() <= 200),
        "bounded in size"
    );
    // 2. Words the runtime wrote from a path: a fetch error names the file, and the file is under a home directory
    //    whose name carries the canary.
    let nested = tempfile::tempdir().expect("tmp");
    let root = nested
        .path()
        .join("Users/jane.doe")
        .join(CANARY)
        .join("packs");
    std::fs::create_dir_all(&root).expect("dirs");
    let fetched = f.installer.clone().with_builtin_root(&root);
    let report2 = match fetched
        .install(
            TENANT,
            request(3, PackSource::Builtin("coding".to_string())),
        )
        .await
    {
        Err(InstallError::Failed(report)) => *report,
        other => panic!("{other:?}"),
    };
    assert_eq!(report2.phase, Phase::Fetch);
    assert!(
        report2.diagnostics[0].message.contains("~"),
        "{}",
        report2.diagnostics[0].message
    );
    // 3. Text a person chose for a name is not stored as one.
    let mut named = coding();
    named["name"] = json!(format!("sk-live-{CANARY}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    let report3 = f.failed(4, inline(named)).await;
    assert_eq!(report3.pack_name, "(invalid name)");
    // Nothing reached the stored rows or what a failure shows.
    let stored = stored_text(&f).await;
    let shown = format!("{report:?}{report2:?}{report3:?}{}", report.to_json());
    for leak in LEAKS {
        assert!(
            !stored.contains(leak),
            "{leak} reached the stored report: {stored}"
        );
        assert!(!shown.contains(leak), "{leak} reached what a failure shows");
    }
    assert!(
        stored.contains("[REDACTED"),
        "the redaction is visible: {stored}"
    );
    f.finish().await;
}

#[tokio::test]
async fn reports_expire_are_purged_and_belong_to_their_tenant() {
    let Some(f) = fixture("adp008_expiry", |config| config.reports.retention_days = 1).await else {
        common::blocked_marker();
        return;
    };
    let installed = f.installed(1, coding()).await;
    let report = &installed.report;
    // A report is kept for the configured period and not a minute longer than it says.
    let lifetime = report.expires_at - report.started_at;
    assert!(
        (lifetime - chrono::Duration::days(1)).num_seconds().abs() < 60,
        "{lifetime}"
    );
    assert_eq!(
        f.store
            .reports(TENANT, None, 50)
            .await
            .expect("reports")
            .len(),
        1
    );
    // Another tenant sees none of it.
    assert!(f
        .store
        .reports(OTHER_TENANT, None, 50)
        .await
        .expect("other")
        .is_empty());
    assert!(f.store.packs(OTHER_TENANT).await.expect("other").is_empty());

    // Past its expiry it is not returned, and the next read or install purges the row.
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("UPDATE pack_install_reports SET expires_at = now() - interval '1 minute'")
        .execute(&mut *tx)
        .await
        .expect("age it");
    tx.commit().await.expect("commit");
    assert!(
        f.store
            .report(TENANT, &command(1))
            .await
            .expect("read")
            .is_none(),
        "an expired report is not returned"
    );
    assert_eq!(
        f.rows("SELECT count(*) FROM pack_install_reports").await,
        1,
        "not yet purged"
    );
    assert!(f
        .store
        .reports(TENANT, None, 50)
        .await
        .expect("reports")
        .is_empty());
    assert_eq!(
        f.rows("SELECT count(*) FROM pack_install_reports").await,
        0,
        "purged by the sweep"
    );
    // Installing is what sweeps too: age a new one, install another, and the old row is gone.
    f.installed(2, edited(|m| m["semver"] = json!("1.1.0")))
        .await;
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("UPDATE pack_install_reports SET expires_at = now() - interval '1 minute'")
        .execute(&mut *tx)
        .await
        .expect("age it");
    tx.commit().await.expect("commit");
    f.installed(3, edited(|m| m["semver"] = json!("1.2.0")))
        .await;
    assert_eq!(f.rows("SELECT count(*) FROM pack_install_reports").await, 1);
    f.finish().await;
}

#[tokio::test]
async fn the_whole_flow_runs_as_the_least_privileged_role_under_row_level_security() {
    let Some(f) = fixture("adp008_rls", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    use sqlx::postgres::PgPoolOptions;
    use sqlx::Executor;
    let app = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|connection, _| {
            Box::pin(async move {
                connection.execute("SET ROLE quansio_app").await?;
                Ok(())
            })
        })
        .connect(&common::scratch_url(&f.name))
        .await
        .expect("an app-role pool");
    let store = PackStore::new(app.clone(), Arc::new(f.config.clone()));
    let installer = PackInstaller::new(
        store.clone(),
        Some(Arc::new(GatewayPackCompiler::new(
            f.client.clone(),
            Duration::from_secs(30),
        ))),
    );
    installer
        .install(TENANT, request(1, inline(coding())))
        .await
        .expect("installed as quansio_app");
    let failed = installer
        .install(
            TENANT,
            request(
                2,
                inline(edited(|m| m["contents"]["tool_names"] = json!([HOST_ONLY]))),
            ),
        )
        .await;
    assert!(matches!(failed, Err(InstallError::Failed(_))));
    assert_eq!(
        store
            .reports(TENANT, None, 50)
            .await
            .expect("reports")
            .len(),
        2
    );
    store
        .rollback(
            TENANT,
            "coding",
            None,
            &Actor::user(USER),
            CorrelationId::generate(&mut UlidGenerator::new()),
        )
        .await
        .expect_err("a single revision has nothing to roll back to");
    // With no tenant context the role sees nothing, in either table; the superuser sees both rows.
    let bare_reports: i64 = sqlx::query_scalar("SELECT count(*) FROM pack_install_reports")
        .fetch_one(&app)
        .await
        .expect("bare");
    let bare_packs: i64 = sqlx::query_scalar("SELECT count(*) FROM capability_pack_versions")
        .fetch_one(&app)
        .await
        .expect("bare");
    assert_eq!((bare_reports, bare_packs), (0, 0));
    let all: i64 = sqlx::query_scalar("SELECT count(*) FROM pack_install_reports")
        .fetch_one(&f.pool)
        .await
        .expect("super");
    assert_eq!(all, 2);
    assert!(store
        .reports(OTHER_TENANT, None, 50)
        .await
        .expect("other")
        .is_empty());
    f.finish().await;
}

// ---- over the API ---------------------------------------------------------------------------------

fn http(
    method: &str,
    path: &str,
    body: Option<Value>,
    user: &str,
    tenant: &str,
) -> Request<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-quansio-tenant", tenant)
        .header("x-quansio-user", user);
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(axum::body::Body::empty()).expect("request"),
    }
}

async fn send(router: &axum::Router, request: Request<axum::body::Body>) -> (u16, String, Value) {
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&bytes).to_string();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, text, json)
}

#[tokio::test]
async fn the_commands_and_the_read_work_over_the_api_and_leak_nothing() {
    let Some(f) = fixture("adp008_api", |_| {}).await else {
        common::blocked_marker();
        return;
    };
    let state = ApiState::new(f.pool.clone())
        .expect("api")
        .with_intelligence(f.client.clone());
    let router = quansio_server::api::router(state);
    let install = |n: u32, params: Value| {
        send(
            &router,
            http(
                "POST",
                "/v1/commands/InstallPack",
                Some(json!({ "command_id": command(n), "params": params })),
                USER,
                TENANT,
            ),
        )
    };

    // An install, then the same command again (the owner's answer replays), then a rollback with nothing to roll back to.
    let source = json!({ "kind": "inline", "manifest": coding() });
    let (status, _, ok) = install(1, json!({ "workspace_id": WORKSPACE, "source": source })).await;
    assert_eq!(status, 200, "{ok}");
    assert_eq!(ok["result"]["outcome"], "activated");
    assert_eq!(ok["result"]["revision"], revision_of(&coding()));
    let (status, _, replay) =
        install(1, json!({ "workspace_id": WORKSPACE, "source": source })).await;
    assert_eq!(status, 200);
    assert_eq!(replay["replayed"], true);
    let (status, _, none) = send(
        &router,
        http(
            "POST",
            "/v1/commands/RollbackPack",
            Some(json!({ "command_id": command(2), "params": { "pack": "coding" } })),
            USER,
            TENANT,
        ),
    )
    .await;
    assert_eq!(status, 409, "{none}");
    assert_eq!(none["code"], "CONFLICT_STATE");

    // A stale expectation is CONFLICT_REVISION, and the failure is a report either way.
    let (status, _, stale) = install(
        3,
        json!({ "workspace_id": WORKSPACE, "source": { "kind": "inline", "manifest": edited(|m| m["semver"] = json!("1.1.0")) },
                "expected_active_revision": null }),
    )
    .await;
    assert_eq!(status, 409, "{stale}");
    assert_eq!(stale["code"], "CONFLICT_REVISION");

    // A failed install is PACK_INSTALL_FAILED carrying the phase, the outcome and redacted findings. The file path is
    // the person's text and the plane quotes it back; nothing of it may come out.
    let bad_path = format!("../https://deploy:pw-{CANARY}@packs.test/p?token=abc123token");
    let (status, text, failed) = install(
        4,
        json!({ "workspace_id": WORKSPACE, "source": { "kind": "inline", "manifest": coding(), "files": { bad_path: "x" } } }),
    )
    .await;
    assert_eq!(status, 422, "{failed}");
    assert_eq!(failed["code"], "PACK_INSTALL_FAILED");
    assert_eq!(failed["details"]["phase"], "verify");
    assert_eq!(failed["details"]["outcome"], "retained_previous");
    for leak in LEAKS {
        assert!(
            !text.contains(leak),
            "{leak} reached the API answer: {text}"
        );
    }

    // Malformed requests are the caller's mistake.
    for params in [
        json!({ "source": source }),
        json!({ "workspace_id": WORKSPACE }),
        json!({ "workspace_id": WORKSPACE, "source": { "kind": "inline", "manifest": {}, "extra": 1 } }),
        json!({ "workspace_id": WORKSPACE, "source": source, "expected_revision": "nope" }),
    ]
    .into_iter()
    .zip(20..)
    {
        let (status, _, body) = install(params.1, params.0).await;
        assert!(status == 400, "{status} {body}");
    }

    // The read: packs for any member, reports for an administrator, redacted.
    let (status, text, read) = send(&router, http("GET", "/v1/packs", None, USER, TENANT)).await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["packs"][0]["name"], "coding");
    assert_eq!(
        read["packs"][0]["active"]["revision"],
        revision_of(&coding())
    );
    let reports = read["install_reports"]
        .as_array()
        .expect("an administrator sees the reports");
    assert!(reports.len() >= 3, "{read}");
    let newest = &reports[0];
    assert_eq!(
        (
            newest["state"].as_str(),
            newest["phase"].as_str(),
            newest["outcome"].as_str()
        ),
        (Some("failed"), Some("verify"), Some("retained_previous"))
    );
    for leak in LEAKS {
        assert!(!text.contains(leak), "{leak} reached the read: {text}");
    }
    let (_, _, filtered) = send(
        &router,
        http("GET", "/v1/packs?pack=nothing", None, USER, TENANT),
    )
    .await;
    assert_eq!(filtered["install_reports"], json!([]));

    // Another tenant's administrator sees neither the packs nor the reports.
    let (_, _, other) = send(
        &router,
        http("GET", "/v1/packs", None, OTHER_USER, OTHER_TENANT),
    )
    .await;
    assert_eq!(other["packs"], json!([]));
    assert_eq!(other["install_reports"], json!([]));

    // A member who cannot manage the tenant installs nothing and reads no reports.
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "UPDATE tenant_memberships SET role = 'member' WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(TENANT)
    .bind(USER)
    .execute(&mut *tx)
    .await
    .expect("demote");
    tx.commit().await.expect("commit");
    let (status, _, denied) =
        install(10, json!({ "workspace_id": WORKSPACE, "source": source })).await;
    assert_eq!(status, 403, "{denied}");
    let (_, _, member) = send(&router, http("GET", "/v1/packs", None, USER, TENANT)).await;
    assert_eq!(member["install_reports"], Value::Null);
    assert_eq!(member["packs"][0]["name"], "coding");
    f.finish().await;
}
