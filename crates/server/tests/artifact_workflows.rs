//! CAP-003: artifacts are made and edited through governed tools, as versioned source files,
//! and a malformed deliverable blocks completion — against a scratch PostgreSQL database and
//! the development MinIO (blocked, not skipped, without them).
//!
//! * the source corpus (`packs/skills/artifacts/fixtures/corpus.yaml`) gets the same verdicts
//!   here as in the intelligence plane (`python/tests/intelligence/test_artifact_tool_calls.py`);
//! * a run creates a document with `artifact.create`, edits it with `artifact.update` (a new
//!   immutable version whose parent is the first, bytes verified from the object store), has a
//!   malformed edit and a digest mismatch refused by the host with no version created, and
//!   completes through `artifact_exists` + `artifact_valid`;
//! * the fixtures for every editable kind go through the same tool path;
//! * a malformed deliverable — uploaded rather than made by the tool — fails `artifact_valid`,
//!   so the run does not complete until a valid version is added.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `QUANSIO_TEST_MINIO_*` (see `tests/artifacts.rs`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{CanonicalId, CorrelationId, Digest, Prefix, UlidGenerator};
use quansio_server::artifacts::formats;
use quansio_server::artifacts::tool_host::{ArtifactToolHost, ServerToolHost};
use quansio_server::artifacts::{
    ArtifactGrant, ArtifactId, ArtifactKind, ArtifactStore, GrantLevel, NewArtifact, NewVersion,
    ObjectStore, Origin, OriginKind, ProducedBy, Retention, S3ObjectStore,
};
use quansio_server::control::schema;
use quansio_server::policy::{ActorRoles, TenantRole, WorkspaceRole};
use quansio_server::runtime::agents::AgentDelegationPort;
use quansio_server::runtime::state_machine::{
    CompletionClaim, DelegationPort, ModelCallRequest, ModelProposal, ModelProposalSource, NewRun,
    ProposedToolCall, Run, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity,
    TurnInput,
};
use quansio_server::runtime::turn_loop::{
    ActingActor, RoleProvider, StoredProjectionProvider, ToolDispatchService, UnavailableToolHost,
};
use quansio_server::runtime::verification::ContractVerifier;
use serde_json::{json, Value};
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YCAP003";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YCAP003";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YCAP003";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YCAP003";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9YCAP003";

fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/skills/artifacts/fixtures")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).expect("fixture")
}

// ---------------------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------------------

fn kind_of(name: &str) -> ArtifactKind {
    serde_json::from_value(Value::String(name.to_string())).expect("kind")
}

#[test]
fn the_source_corpus_verdicts_are_the_same_as_the_intelligence_planes() {
    let corpus: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(fixtures().join("corpus.yaml")).expect("corpus"),
    )
    .expect("yaml");
    let sources = corpus["sources"].as_sequence().expect("sources");
    assert!(sources.len() >= 15);
    for entry in sources {
        let file = entry["file"].as_str().expect("file");
        let kind = kind_of(entry["kind"].as_str().expect("kind"));
        let media_type = entry["media_type"].as_str().expect("media type");
        let check = formats::check(kind, media_type, &fixture(file));
        let expected = entry["ok"].as_bool().expect("ok");
        assert_eq!(check.ok, expected, "{file} as {media_type}: {check:?}");
        if !expected {
            assert_eq!(
                check.rule,
                entry["rule"].as_str().expect("rule"),
                "{file}: {check:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

/// What the scripted model does on each call.
#[derive(Clone)]
enum Move {
    Create {
        kind: &'static str,
        title: &'static str,
        media_type: &'static str,
        source: Vec<u8>,
    },
    /// Edit the workspace's latest `report` artifact.
    Update {
        source: Vec<u8>,
        digest: Option<String>,
    },
    Claim,
}

/// A model that plays scripted moves; an update looks up the artifact the run made, the way a
/// model reads the id from the create call's result.
#[derive(Clone)]
struct ScriptedModel {
    pool: PgPool,
    moves: Arc<Mutex<VecDeque<Move>>>,
}

fn call(id: &str, tool: &str, arguments: Value) -> ModelProposal {
    ModelProposal {
        tool_calls: vec![ProposedToolCall::new(id, tool, arguments)],
        ..ModelProposal::default()
    }
}

fn content_arguments(kind: ArtifactKind, source: &[u8]) -> Value {
    let digest = Digest::of(source).to_string();
    if formats::is_editable(kind) {
        json!({ "content": String::from_utf8(source.to_vec()).expect("utf-8"), "content_digest": digest })
    } else {
        use base64::Engine as _;
        json!({
            "content_base64": base64::engine::general_purpose::STANDARD.encode(source),
            "content_digest": digest,
        })
    }
}

async fn latest_report(pool: &PgPool) -> (String, ArtifactKind) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let (id, kind): (String, String) = sqlx::query_as(
        "SELECT id, kind FROM artifacts WHERE tenant_id = $1 AND role = 'report' \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(TENANT)
    .fetch_one(&mut *tx)
    .await
    .expect("report artifact");
    tx.commit().await.expect("commit");
    (id, kind_of(&kind))
}

#[async_trait]
impl ModelProposalSource for ScriptedModel {
    async fn propose(&self, _request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let next = self.moves.lock().expect("moves").pop_front();
        let n = UlidGenerator::new().generate().to_string();
        Ok(match next {
            Some(Move::Create {
                kind,
                title,
                media_type,
                source,
            }) => {
                let mut arguments = content_arguments(kind_of(kind), &source);
                arguments["role"] = json!("report");
                arguments["kind"] = json!(kind);
                arguments["title"] = json!(title);
                arguments["media_type"] = json!(media_type);
                call(&format!("call_{n}"), "artifact.create", arguments)
            }
            Some(Move::Update { source, digest }) => {
                let (artifact_id, kind) = latest_report(&self.pool).await;
                let mut arguments = content_arguments(kind, &source);
                if let Some(digest) = digest {
                    arguments["content_digest"] = json!(digest);
                }
                arguments["artifact_id"] = json!(artifact_id);
                call(&format!("call_{n}"), "artifact.update", arguments)
            }
            Some(Move::Claim) => ModelProposal {
                completion_claim: Some(CompletionClaim {
                    claim_id: format!("claim_{n}"),
                    summary: "the report is written".to_string(),
                    evidence_ids: Vec::new(),
                }),
                ..ModelProposal::default()
            },
            None => ModelProposal {
                assistant_text: Some("done".to_string()),
                ..ModelProposal::default()
            },
        })
    }
}

struct Roles;

#[async_trait]
impl RoleProvider for Roles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
            user_id: Some(USER.to_string()),
        }))
    }
}

struct Fixture {
    name: String,
    pool: PgPool,
    identity: RuntimeIdentity,
    objects: Arc<dyn ObjectStore>,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(AGENT)
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9YCAP003', $1, NULL, 'tenant', $2)",
    )
    .bind(TENANT)
    .bind(json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" },
        { "effect_class": "content.upload", "resource": { "kind": "artifact", "selector": "*" }, "decision": "allow" }
    ]))
    .execute(&mut *tx)
    .await
    .expect("policy");
    sqlx::query("UPDATE work_nodes SET completion_contract = $1 WHERE id = $2 AND tenant_id = $3")
        .bind(json!({
            "deterministic_checks": [
                { "kind": "artifact_exists", "artifact_role": "report", "min_versions": 1 },
                { "kind": "artifact_valid", "artifact_role": "report" }
            ]
        }))
        .bind(WORK_NODE)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("contract");
    tx.commit().await.expect("commit");
    let identity = RuntimeIdentity::system(
        TENANT,
        "artifact-workflows-test",
        CorrelationId::generate(&mut UlidGenerator::new()),
    );
    let objects: Arc<dyn ObjectStore> =
        Arc::new(S3ObjectStore::from_env().expect("QUANSIO_TEST_MINIO_* configuration"));
    Some(Fixture {
        name,
        pool,
        identity,
        objects,
    })
}

async fn seed_projection(fixture: &Fixture, run_id: &str) {
    let grants: Vec<Grant> = [
        ("read.internal", "fs", "**"),
        ("content.upload", "artifact", "*"),
    ]
    .into_iter()
    .map(|(class, kind, selector)| {
        Grant::from_json(&json!({
            "effect_class": class,
            "resource": { "kind": kind, "selector": selector },
            "constraints": {},
        }))
        .expect("grant")
    })
    .collect();
    let projection = CapabilityProjection::from_parts(
        CanonicalId::generate(Prefix::CapabilityProjection, &mut UlidGenerator::new()),
        ProjectionSubject::new(SubjectKind::Run, run_id),
        Vec::new(),
        grants,
        chrono::Utc::now(),
        Some(chrono::Utc::now() + chrono::Duration::hours(1)),
    )
    .expect("projection");
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO capability_projections (id, tenant_id, workspace_id, subject_kind, \
         subject_id, inputs, grants, inputs_digest, computed_at, expires_at) \
         VALUES ($1, $2, $3, 'run', $4, $5, $6, $7, $8, $9)",
    )
    .bind(projection.id.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(run_id)
    .bind(serde_json::to_value(&projection.inputs).expect("inputs"))
    .bind(serde_json::to_value(&projection.grants).expect("grants"))
    .bind(projection.inputs_digest.to_string())
    .bind(projection.computed_at)
    .bind(projection.expires_at)
    .execute(&mut *tx)
    .await
    .expect("projection row");
    tx.commit().await.expect("commit");
}

/// An engine whose tool host is the deployable's: artifact tools on the server, the rest
/// unavailable.
fn engine(fixture: &Fixture, moves: Vec<Move>) -> RuntimeEngine {
    let delegation: Arc<dyn DelegationPort> = Arc::new(
        AgentDelegationPort::with_structural_check(fixture.pool.clone(), fixture.identity.clone())
            .expect("delegation"),
    );
    let host = ServerToolHost::new(
        ArtifactToolHost::new(fixture.pool.clone(), Arc::clone(&fixture.objects)),
        Arc::new(UnavailableToolHost),
    );
    let dispatcher = ToolDispatchService::new(
        fixture.pool.clone(),
        fixture.identity.clone(),
        Arc::new(
            StoredProjectionProvider::new(fixture.pool.clone(), fixture.identity.clone())
                .expect("projections"),
        ),
        Arc::new(Roles),
        Arc::new(host),
        Arc::clone(&delegation),
    )
    .expect("dispatch");
    RuntimeEngine::new(fixture.pool.clone(), fixture.identity.clone())
        .expect("engine")
        .with_model_source(Arc::new(ScriptedModel {
            pool: fixture.pool.clone(),
            moves: Arc::new(Mutex::new(moves.into())),
        }))
        .with_tool_dispatch(Arc::new(dispatcher))
        .with_delegation(delegation)
        .with_verification(Arc::new(
            ContractVerifier::new(fixture.pool.clone(), fixture.identity.clone())
                .expect("verifier"),
        ))
}

async fn start(fixture: &Fixture, engine: &RuntimeEngine) -> Run {
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
    seed_projection(fixture, &run.id.to_string()).await;
    run
}

async fn turn(engine: &RuntimeEngine, run: &Run) -> RunStatus {
    let current = engine.store().load_run(&run.id).await.expect("run");
    let _ = engine
        .run_turn(
            &run.id,
            current.generation,
            TurnInput::new(RunTriggerKind::Manual),
        )
        .await
        .expect("turn");
    engine.store().load_run(&run.id).await.expect("run").status
}

/// `(tool, status, result)` of the run's tool calls, oldest first.
async fn tool_calls(pool: &PgPool) -> Vec<(String, String, Value)> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query_as::<_, (String, String, Option<Value>)>(
        "SELECT tool_name, status, result FROM tool_calls WHERE tenant_id = $1 ORDER BY created_at, id",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("tool calls");
    tx.commit().await.expect("commit");
    rows.into_iter()
        .map(|(tool, status, result)| (tool, status, result.unwrap_or(Value::Null)))
        .collect()
}

/// `(seq, parent, format_check)` of an artifact's versions.
async fn versions(pool: &PgPool, artifact_id: &str) -> Vec<(i32, Option<String>, String, Value)> {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let rows = sqlx::query_as::<_, (i32, Option<String>, String, Option<Value>)>(
        "SELECT seq, parent_version_id, id, format_check FROM artifact_versions \
         WHERE tenant_id = $1 AND artifact_id = $2 ORDER BY seq",
    )
    .bind(TENANT)
    .bind(artifact_id)
    .fetch_all(&mut *tx)
    .await
    .expect("versions");
    tx.commit().await.expect("commit");
    rows.into_iter()
        .map(|(seq, parent, id, check)| (seq, parent, id, check.unwrap_or(Value::Null)))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_run_creates_and_edits_a_versioned_artifact_through_the_governed_tools() {
    let Some(f) = prepare("cap003_edit").await else {
        blocked_marker();
        return;
    };
    let first = fixture("document.md");
    let edited = [
        first.clone(),
        b"\n## Follow-up\n\nThe second version adds this section.\n".to_vec(),
    ]
    .concat();
    let engine = engine(
        &f,
        vec![
            Move::Create {
                kind: "document",
                title: "Quarterly brief",
                media_type: "text/markdown",
                source: first.clone(),
            },
            Move::Update {
                source: edited.clone(),
                digest: None,
            },
            // A malformed edit and a source that is not what its digest says: both refused.
            Move::Update {
                source: fixture("malformed/document-no-heading.md"),
                digest: None,
            },
            Move::Update {
                source: edited.clone(),
                digest: Some(Digest::of(b"something else").to_string()),
            },
            Move::Claim,
        ],
    );
    let run = start(&f, &engine).await;
    let status = turn(&engine, &run).await;

    let calls = tool_calls(&f.pool).await;
    assert_eq!(calls.len(), 4, "{calls:#?}");
    assert_eq!(
        (calls[0].0.as_str(), calls[0].1.as_str()),
        ("artifact.create", "completed"),
        "{calls:#?}"
    );
    assert_eq!(
        (calls[1].0.as_str(), calls[1].1.as_str()),
        ("artifact.update", "completed"),
        "{calls:#?}"
    );
    let artifact_id = calls[0].2["artifact_id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| panic!("the create call returned the artifact id: {:#?}", calls[0]));
    let refused = |index: usize, rule: &str| {
        let (_, status, result) = &calls[index];
        assert_ne!(
            status, "completed",
            "call {index} must be refused: {result}"
        );
        assert!(
            result.to_string().contains(rule),
            "call {index} names {rule}: {result}"
        );
    };
    refused(2, "artifact.format");
    refused(3, "artifact.content_digest");

    // Two immutable versions, the edit on top of the first; the refused calls added none.
    let history = versions(&f.pool, &artifact_id).await;
    assert_eq!(history.len(), 2, "{history:#?}");
    assert_eq!(history[0].0, 1);
    assert_eq!(
        history[1].1.as_deref(),
        Some(history[0].2.as_str()),
        "v2's parent is v1"
    );
    for (_, _, _, check) in &history {
        assert_eq!(check["ok"], true, "{check}");
    }
    // The bytes are the sources, verified against their digests on the way out.
    let store = ArtifactStore::new(f.objects.as_ref());
    let mut conn = f.pool.acquire().await.expect("conn");
    let id = ArtifactId::parse(&artifact_id).expect("id");
    let artifact = store
        .load(&mut conn, TENANT, USER, &id)
        .await
        .expect("the acting user can read it");
    assert_eq!(artifact.role.as_deref(), Some("report"));
    assert_eq!(artifact.origin.kind, OriginKind::Run);
    for (seq, expected) in [(1, &first), (2, &edited)] {
        let version_id = &history[seq - 1].2;
        let version =
            quansio_server::artifacts::ArtifactVersionId::parse(version_id).expect("version id");
        let bytes = store
            .fetch_bytes(&mut conn, TENANT, USER, &version)
            .await
            .expect("bytes");
        assert_eq!(&bytes, expected, "version {seq} holds its source");
    }
    drop(conn);

    // The claim is verified: the report exists and its current version is valid.
    assert_eq!(status, RunStatus::Succeeded, "{calls:#?}");
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn every_editable_kind_is_made_through_the_tool_from_its_fixture() {
    let Some(f) = prepare("cap003_kinds").await else {
        blocked_marker();
        return;
    };
    let kinds = [
        ("document", "text/markdown", "document.md"),
        ("spreadsheet", "text/csv", "spreadsheet.csv"),
        (
            "presentation",
            "application/vnd.quansio.slides+json",
            "slides.json",
        ),
        ("code", "application/vnd.quansio.code+json", "code.json"),
        ("data", "application/json", "data.json"),
    ];
    let moves = kinds
        .iter()
        .map(|(kind, media_type, file)| Move::Create {
            kind,
            title: "fixture",
            media_type,
            source: fixture(file),
        })
        .collect();
    let engine = engine(&f, moves);
    let run = start(&f, &engine).await;
    turn(&engine, &run).await;
    let calls = tool_calls(&f.pool).await;
    assert_eq!(calls.len(), kinds.len(), "{calls:#?}");
    for (index, (tool, status, result)) in calls.iter().enumerate() {
        assert_eq!(
            (tool.as_str(), status.as_str()),
            ("artifact.create", "completed"),
            "{}: {result}",
            kinds[index].2
        );
    }
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    let made: Vec<(String, Option<Value>)> = sqlx::query_as(
        "SELECT a.kind, v.format_check FROM artifacts a JOIN artifact_versions v ON v.id = a.current_version_id \
         WHERE a.tenant_id = $1 ORDER BY a.created_at, a.id",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("artifacts");
    tx.commit().await.expect("commit");
    let made_kinds: Vec<&str> = made.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        made_kinds,
        kinds.iter().map(|(kind, _, _)| *kind).collect::<Vec<_>>()
    );
    assert!(made
        .iter()
        .all(|(_, check)| check.as_ref().is_some_and(|check| check["ok"] == true)));
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_malformed_deliverable_blocks_completion_until_a_valid_version_is_added() {
    let Some(f) = prepare("cap003_blocked").await else {
        blocked_marker();
        return;
    };
    // A person uploads a malformed spreadsheet as the report: stored, but its check fails.
    let store = ArtifactStore::new(f.objects.as_ref());
    let mut conn = f.pool.acquire().await.expect("conn");
    let uploaded = store
        .create(
            &mut conn,
            TENANT,
            USER,
            &NewArtifact {
                workspace_id: WORKSPACE.to_string(),
                kind: ArtifactKind::Spreadsheet,
                title: "metrics".to_string(),
                role: Some("report".to_string()),
                origin: Origin {
                    kind: OriginKind::Upload,
                    reference: USER.to_string(),
                },
                grants: vec![ArtifactGrant {
                    principal: USER.to_string(),
                    level: GrantLevel::Write,
                }],
                retention: Retention::default(),
            },
        )
        .await
        .expect("artifact");
    let stored = store
        .add_version(
            &mut conn,
            TENANT,
            USER,
            &uploaded.id,
            &NewVersion {
                bytes: fixture("malformed/spreadsheet-ragged.csv"),
                media_type: "text/csv".to_string(),
                expected_digest: None,
                produced_by: ProducedBy {
                    run_id: None,
                    step_id: None,
                    user_id: Some(USER.to_string()),
                },
                quality_record: None,
            },
        )
        .await
        .expect("uploads are stored either way");
    assert_eq!(
        stored
            .version
            .format_check
            .as_ref()
            .map(|check| check["ok"].clone()),
        Some(json!(false))
    );
    drop(conn);

    // The claim fails verification on artifact_valid; the run is not done.
    let engine = engine(&f, vec![Move::Claim]);
    let run = start(&f, &engine).await;
    let status = turn(&engine, &run).await;
    assert_ne!(
        status,
        RunStatus::Succeeded,
        "a malformed deliverable must block completion"
    );
    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    // The verify step's failure carries the feedback (RUN-008).
    let failed: Vec<Value> = sqlx::query_scalar(
        "SELECT a.error FROM attempts a JOIN steps s ON s.id = a.step_id \
         WHERE s.kind = 'verify' AND s.status = 'failed' AND a.tenant_id = $1 AND a.error IS NOT NULL",
    )
    .bind(TENANT)
    .fetch_all(&mut *tx)
    .await
    .expect("events");
    tx.commit().await.expect("commit");
    assert!(
        failed
            .iter()
            .any(|payload| payload.to_string().contains("artifact.format")),
        "the failure names the format rule: {failed:#?}"
    );

    // A valid version through the tool, then the claim passes.
    let engine = engine_after(
        &f,
        vec![
            Move::Update {
                source: fixture("spreadsheet.csv"),
                digest: None,
            },
            Move::Claim,
        ],
    );
    let status = turn(&engine, &run).await;
    assert_eq!(
        status,
        RunStatus::Succeeded,
        "{:#?}",
        tool_calls(&f.pool).await
    );
    let history = versions(&f.pool, &uploaded.id.to_canonical()).await;
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].3["ok"], true);
    drop_pool(&f.pool, &f.name).await;
}

/// A fresh engine for a later turn of the same run.
fn engine_after(fixture: &Fixture, moves: Vec<Move>) -> RuntimeEngine {
    engine(fixture, moves)
}
