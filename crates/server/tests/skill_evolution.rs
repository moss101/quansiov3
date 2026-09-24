//! CAP-008: controlled skill evolution through the real intelligence plane.
//!
//! Verified run outcomes in canonical state (runs, their evidence, the skill versions their
//! recorded context projections named) → `ProposeSkillCandidates` (tenant administrators) → the
//! Python plane detects the pattern, proposes a patch and evaluates it → the registry stores a
//! CANDIDATE whose provenance names the evidence and the evaluation. Production never changes: the
//! ACTIVE version stays, no registry tool can write a skill, and an evolution candidate cannot be
//! promoted past EVALUATING without a passing evaluation.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`, `uv`, and the dev stub provider (the plane's service
//! is started by `common::intelligence`); without them the test prints `BLOCKED_EXTERNAL`.

use quansio_core::{CanonicalId, CorrelationId, UlidGenerator};
use quansio_events::Actor;
use quansio_server::control::schema;
use quansio_server::control::skills::{
    NewSkill, NewSkillVersion, SkillAdmin, SkillScope, SkillStatus, SkillStore,
};
use quansio_server::intelligence::IntelligenceClient;
use quansio_tools::ToolRegistry;
use serde_json::{json, Value};

mod common;

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YCAP008";
const OWNER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YCAP008";
const MEMBER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YCAPMBR";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YCAP008";
const NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YCAP008";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9YCAP008";

fn admin() -> SkillAdmin {
    SkillAdmin::new(
        TENANT,
        Actor::system("skill-evolution-test"),
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

async fn active_notes_skill(pool: &sqlx::PgPool) -> (CanonicalId, CanonicalId) {
    let store = SkillStore::new(pool.clone());
    let skill = store
        .create_skill(
            &admin(),
            NewSkill {
                scope: SkillScope::Tenant,
                name: "notes-reader".to_string(),
                owner: "ai-platform".to_string(),
            },
        )
        .await
        .expect("skill");
    let version = store
        .create_version(
            &admin(),
            &skill.id,
            NewSkillVersion {
                semver: "1.0.0".to_string(),
                manifest: json!({
                    "instructions": "Read the file, then quote what matters.",
                    "examples": ["read the notes"],
                    "tool_needs": ["fs.read"],
                    "capability_needs": ["read.internal"],
                }),
                provenance: Some("authored".to_string()),
            },
        )
        .await
        .expect("version");
    for step in [
        SkillStatus::Candidate,
        SkillStatus::Evaluating,
        SkillStatus::Approved,
        SkillStatus::Active,
    ] {
        store
            .promote(&admin(), &version.id, step)
            .await
            .expect("promote");
    }
    (skill.id, version.id)
}

/// Three runs that FAILED with the skill in their context, each with its evidence.
async fn failed_runs(pool: &sqlx::PgPool, version_id: &str) -> Vec<String> {
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
    let mut evidence_ids = Vec::new();
    for index in 1..=3 {
        let suffix = format!("01J8Z3K6F1N8VQ2X5W9YCAPR{index:02}");
        let run = format!("run_{suffix}");
        sqlx::query(
            "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, status, \
                               trigger_kind, terminal_reason) \
             VALUES ($1, $2, $3, $4, $5, 'FAILED', 'manual', 'the notes file was missing')",
        )
        .bind(&run)
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(NODE)
        .bind(AGENT)
        .execute(&mut *tx)
        .await
        .expect("run");
        sqlx::query(
            "INSERT INTO context_projections (id, tenant_id, run_id, turn_id, segments) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(format!("ctx_{suffix}"))
        .bind(TENANT)
        .bind(&run)
        .bind(format!("trn_{suffix}"))
        .bind(json!([
            {"segment_id": "seg_0", "kind": "system", "source": "config", "trust_level": "trusted_system", "tokens": 5},
            {"segment_id": "seg_1", "kind": "skill", "source": version_id, "trust_level": "verified_knowledge", "tokens": 9},
        ]))
        .execute(&mut *tx)
        .await
        .expect("projection");
        let evidence = format!("evd_{suffix}");
        sqlx::query(
            "INSERT INTO evidence (id, tenant_id, workspace_id, run_id, kind, content_digest, \
                                   inline_summary, captured_by_kind, captured_by_id) \
             VALUES ($1, $2, $3, $4, 'log', $5, 'fs.read: /work/notes.md not found', 'runtime', 'test')",
        )
        .bind(&evidence)
        .bind(TENANT)
        .bind(WORKSPACE)
        .bind(&run)
        .bind(format!("{index:064x}"))
        .execute(&mut *tx)
        .await
        .expect("evidence");
        evidence_ids.push(evidence);
    }
    tx.commit().await.expect("commit");
    evidence_ids
}

async fn member(pool: &sqlx::PgPool) {
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'member')")
        .bind(MEMBER)
        .bind(format!("{MEMBER}@example.com"))
        .execute(&mut *tx)
        .await
        .expect("user");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query(
        "INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) \
         VALUES ('tm_01J8Z3K6F1N8VQ2X5W9YCAPMBR', $1, $2, 'member', 'active')",
    )
    .bind(TENANT)
    .bind(MEMBER)
    .execute(&mut *tx)
    .await
    .expect("membership");
    sqlx::query(
        "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
         VALUES ('wm_01J8Z3K6F1N8VQ2X5W9YCAPMBR', $1, $2, $3, 'editor', 'active')",
    )
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(MEMBER)
    .execute(&mut *tx)
    .await
    .expect("workspace membership");
    tx.commit().await.expect("commit");
}

async fn command(router: &axum::Router, user: &str, name: &str, params: Value) -> (u16, Value) {
    use tower::ServiceExt;
    let command_id = format!("cmd_{}", UlidGenerator::new().generate().to_base32());
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/v1/commands/{name}"))
                .header("x-quansio-tenant", TENANT)
                .header("x-quansio-user", user)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"command_id": command_id, "params": params}).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}

#[tokio::test]
async fn verified_experience_becomes_an_evaluated_candidate_and_never_production() {
    let Some(service) = common::intelligence::start().await else {
        return;
    };
    let name = common::scratch_name("skill_evolution");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    member(&pool).await;
    let (skill_id, active_id) = active_notes_skill(&pool).await;
    let evidence = failed_runs(&pool, &active_id.to_string()).await;

    let client = IntelligenceClient::connect_lazy(&service.endpoint).expect("endpoint");
    let router = quansio_server::api::router(
        quansio_server::api::ApiState::new(pool.clone())
            .expect("api")
            .with_intelligence(client),
    );

    // Only a tenant administrator may run evolution or promote.
    let (status, body) = command(
        &router,
        MEMBER,
        "ProposeSkillCandidates",
        json!({"workspace_id": WORKSPACE}),
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // The pattern (three verified failures with the skill in context) becomes one CANDIDATE.
    let (status, body) = command(
        &router,
        OWNER,
        "ProposeSkillCandidates",
        json!({"workspace_id": WORKSPACE}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let result = &body["result"];
    assert_eq!(result["records"], 3, "{body}");
    let candidates = result["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1, "{body}");
    let candidate = &candidates[0];
    assert_eq!(candidate["status"], "candidate");
    assert_eq!(candidate["semver"], "1.0.1");
    assert_eq!(
        candidate["resolves"], false,
        "a candidate never enters context"
    );
    let provenance = &candidate["provenance"];
    assert_eq!(provenance["origin"], "evolution");
    assert_eq!(provenance["from_version_id"], active_id.to_string());
    assert_eq!(provenance["pattern_kind"], "failure");
    let linked: Vec<&str> = provenance["evidence"]
        .as_array()
        .expect("evidence")
        .iter()
        .filter_map(|item| item["evidence_id"].as_str())
        .collect();
    assert_eq!(
        linked,
        evidence.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(provenance["evaluation"]["passed"], true, "{provenance}");
    assert!(provenance["evaluation"]["eval_run_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));

    // Production did not change: the same version is still the one ACTIVE version.
    let active = SkillStore::new(pool.clone())
        .active_versions(TENANT)
        .await
        .expect("active");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].1.id, active_id);
    assert_eq!(active[0].0.id, skill_id);

    // Evolving again proposes nothing new.
    let (_, again) = command(
        &router,
        OWNER,
        "ProposeSkillCandidates",
        json!({"workspace_id": WORKSPACE}),
    )
    .await;
    assert_eq!(again["result"]["candidates"], json!([]), "{again}");
    assert_eq!(
        again["result"]["already_proposed"],
        json!([format!("{skill_id}@1.0.1")])
    );

    // Governed promotion: a member cannot; an administrator can, because the evaluation passed.
    let candidate_id = candidate["skill_version_id"]
        .as_str()
        .expect("id")
        .to_string();
    let (status, body) = command(
        &router,
        MEMBER,
        "PromoteSkillVersion",
        json!({"skill_version_id": candidate_id, "to": "evaluating"}),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    for to in ["evaluating", "approved"] {
        let (status, body) = command(
            &router,
            OWNER,
            "PromoteSkillVersion",
            json!({"skill_version_id": candidate_id, "to": to}),
        )
        .await;
        assert_eq!(status, 200, "{to}: {body}");
    }

    common::drop_pool(&pool, &name).await;
    drop(service);
}

#[tokio::test]
async fn an_evolution_candidate_without_a_passing_evaluation_cannot_be_approved() {
    let name = common::scratch_name("skill_evolution_gate");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    common::seed_tenant(&pool, TENANT, OWNER, WORKSPACE, NODE).await;
    let (skill_id, active_id) = active_notes_skill(&pool).await;
    let store = SkillStore::new(pool.clone());
    let router =
        quansio_server::api::router(quansio_server::api::ApiState::new(pool.clone()).expect("api"));

    let evidence = json!([{"evidence_id": "evd_01J8Z3K6F1N8VQ2X5W9YCAPR01", "digest": "a".repeat(64), "run_id": "run_x"}]);
    let cases = [
        (
            "1.0.1",
            json!({"origin": "evolution", "from_version_id": active_id.to_string(), "evidence": evidence,
                         "evaluation": {"eval_run_id": "eval_x", "passed": false, "reason": "skill.regressions 1 > 0"}}),
        ),
        (
            "1.0.2",
            json!({"origin": "evolution", "from_version_id": active_id.to_string(), "evidence": evidence}),
        ),
        (
            "1.0.3",
            json!({"origin": "evolution", "from_version_id": active_id.to_string(), "evidence": [],
                         "evaluation": {"eval_run_id": "eval_y", "passed": true}}),
        ),
    ];
    for (semver, provenance) in cases {
        let version = store
            .create_version(
                &admin(),
                &skill_id,
                NewSkillVersion {
                    semver: semver.to_string(),
                    manifest: json!({"instructions": "x"}),
                    provenance: Some(provenance.to_string()),
                },
            )
            .await
            .expect("version");
        for step in [SkillStatus::Candidate, SkillStatus::Evaluating] {
            store
                .promote(&admin(), &version.id, step)
                .await
                .expect("the ladder below APPROVED is open");
        }
        let (status, body) = command(
            &router,
            OWNER,
            "PromoteSkillVersion",
            json!({"skill_version_id": version.id.to_string(), "to": "approved"}),
        )
        .await;
        assert_eq!(status, 403, "{semver}: {body}");
        assert_eq!(body["code"], "POLICY_DENIED", "{semver}: {body}");
        let refused = store
            .promote(&admin(), &version.id, SkillStatus::Approved)
            .await;
        assert!(
            refused.is_err(),
            "{semver}: the registry itself refuses, not only the API"
        );
        // The one path out of EVALUATING for it is rejection.
        store
            .promote(&admin(), &version.id, SkillStatus::Rejected)
            .await
            .expect("reject");
    }

    // A version authored by a person (free-text provenance) is not gated by evolution.
    let authored = store
        .create_version(
            &admin(),
            &skill_id,
            NewSkillVersion {
                semver: "2.0.0".to_string(),
                manifest: json!({"instructions": "y"}),
                provenance: Some("authored in review".to_string()),
            },
        )
        .await
        .expect("version");
    for step in [
        SkillStatus::Candidate,
        SkillStatus::Evaluating,
        SkillStatus::Approved,
    ] {
        store
            .promote(&admin(), &authored.id, step)
            .await
            .expect("authored ladder");
    }

    // Without the intelligence plane the command fails closed.
    let (status, body) = command(
        &router,
        OWNER,
        "ProposeSkillCandidates",
        json!({"workspace_id": WORKSPACE}),
    )
    .await;
    assert_eq!(body["code"], "TARGET_UNAVAILABLE", "{status}: {body}");
    common::drop_pool(&pool, &name).await;
}

#[test]
fn no_tool_a_run_can_call_writes_a_skill() {
    // A task run acts only through registry tools; none of them names the skill registry.
    let registry = ToolRegistry::builtin();
    for name in registry.callable_names() {
        let declaration = registry.get(name).expect("declared");
        assert!(
            !name.starts_with("skill"),
            "{name} would let a run touch skills"
        );
        for grant in &declaration.grant_templates {
            let class = grant.effect_class.as_str();
            assert!(!class.contains("skill"), "{name} needs {class}");
        }
    }
}
