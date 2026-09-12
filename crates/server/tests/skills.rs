//! Skill registry, promotion ladder and production-context tests (INT-009).
//!
//! They drive the control plane's `skills` store against a real scratch PostgreSQL
//! database and assert DOMAIN.md §11.5: a version starts `DRAFT`, only ladder edges are
//! accepted, a refusal to promote writes neither state nor event, only `ACTIVE` versions
//! reach production context, `skills.current_active_version_id` names exactly the active
//! version, a skill has at most one active version, and the view the resolver consumes is
//! tenant-scoped and deterministically ordered.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{Actor, EventStore, RuntimeEvent};
use quansio_server::control::schema;
use quansio_server::control::skills::{
    NewSkill, NewSkillVersion, SkillAdmin, SkillControlError, SkillScope, SkillStatus, SkillStore,
};
use serde_json::json;
use sqlx::PgPool;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const USER_B: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const WORKSPACE_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const WORK_NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";

/// A scratch database with the canonical schema and two seeded tenants.
struct Fixture {
    name: String,
    pool: PgPool,
    skills: SkillStore,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    seed_tenant(&pool, TENANT_B, USER_B, WORKSPACE_B, WORK_NODE_B).await;
    Some(Fixture {
        name,
        skills: SkillStore::new(pool.clone()),
        pool,
    })
}

fn admin(tenant: &str) -> SkillAdmin {
    SkillAdmin::new(
        tenant,
        Actor::system("skills-test"),
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

fn manifest(triggers: &[&str]) -> serde_json::Value {
    json!({
        "instructions": "Follow the runbook.",
        "examples": [],
        "tool_needs": [],
        "capability_needs": [],
        "eval_suite_id": "evs_01J8Z3K6F1N8VQ2X5W9Y0GGGGG",
        "compatibility": "1.x",
        "recovery_guidance": "",
        "triggers": triggers,
    })
}

async fn new_skill(skills: &SkillStore, tenant: &str, name: &str) -> CanonicalId {
    skills
        .create_skill(
            &admin(tenant),
            NewSkill {
                scope: SkillScope::Tenant,
                name: name.to_string(),
                owner: "ai-platform".to_string(),
            },
        )
        .await
        .expect("create skill")
        .id
}

async fn new_version(
    skills: &SkillStore,
    tenant: &str,
    skill_id: &CanonicalId,
    semver: &str,
) -> CanonicalId {
    skills
        .create_version(
            &admin(tenant),
            skill_id,
            NewSkillVersion {
                semver: semver.to_string(),
                manifest: manifest(&["deploy the release"]),
                provenance: Some("authored in review".to_string()),
            },
        )
        .await
        .expect("create version")
        .id
}

async fn walk_to(
    skills: &SkillStore,
    tenant: &str,
    version: &CanonicalId,
    target: SkillStatus,
) -> Result<(), SkillControlError> {
    let ladder = [
        SkillStatus::Candidate,
        SkillStatus::Evaluating,
        SkillStatus::Approved,
        SkillStatus::Active,
    ];
    for step in ladder {
        skills.promote(&admin(tenant), version, step).await?;
        if step == target {
            break;
        }
    }
    Ok(())
}

async fn events(pool: &PgPool, tenant: &str) -> Vec<RuntimeEvent> {
    EventStore::new(pool.clone())
        .read_events_after(tenant, None, 500)
        .await
        .expect("read events")
}

async fn event_types(pool: &PgPool, tenant: &str) -> Vec<String> {
    events(pool, tenant)
        .await
        .into_iter()
        .map(|event| event.event_type.to_string())
        .collect()
}

#[tokio::test]
async fn a_skill_and_its_first_version_start_in_draft() {
    let Some(fixture) = prepare("skills_draft").await else {
        return blocked_marker();
    };
    let skills = &fixture.skills;

    let skill = skills
        .create_skill(
            &admin(TENANT),
            NewSkill {
                scope: SkillScope::Workspace,
                name: "release-runbook".to_string(),
                owner: "ai-platform".to_string(),
            },
        )
        .await
        .expect("create skill");
    assert!(skill.id.to_string().starts_with("skl_"));
    assert_eq!(skill.scope, SkillScope::Workspace);
    assert_eq!(
        skill.current_active_version_id, None,
        "a new skill has nothing in production"
    );

    let version = skills
        .create_version(
            &admin(TENANT),
            &skill.id,
            NewSkillVersion {
                semver: "1.0.0".to_string(),
                manifest: manifest(&["deploy the release"]),
                provenance: Some("authored in review".to_string()),
            },
        )
        .await
        .expect("create version");
    assert!(version.id.to_string().starts_with("sklv_"));
    assert_eq!(version.status, SkillStatus::Draft);
    assert!(!version.resolves());
    assert_eq!(
        version.manifest["instructions"], "Follow the runbook.",
        "the manifest round-trips through jsonb"
    );
    assert_eq!(
        version.provenance.as_deref(),
        Some("authored in review"),
        "provenance is recorded with the version"
    );

    assert!(
        skills
            .active_versions(TENANT)
            .await
            .expect("read view")
            .is_empty(),
        "nothing is in production before a promotion"
    );
    assert_eq!(
        event_types(&fixture.pool, TENANT).await,
        vec!["skill.created", "skill.version_created"]
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn an_unapproved_version_cannot_enter_production_context() {
    let Some(fixture) = prepare("skills_unapproved").await else {
        return blocked_marker();
    };
    let skills = &fixture.skills;
    let skill = new_skill(skills, TENANT, "release-runbook").await;
    let version = new_version(skills, TENANT, &skill, "1.0.0").await;
    let before = event_types(&fixture.pool, TENANT).await;

    for target in [
        SkillStatus::Active,
        SkillStatus::Retired,
        SkillStatus::Approved,
    ] {
        let error = skills
            .promote(&admin(TENANT), &version, target)
            .await
            .expect_err("an edge the ladder does not have");
        assert_eq!(
            error.code(),
            "RUNTIME_ILLEGAL_TRANSITION",
            "draft → {target:?} is not an edge"
        );
    }

    // A version under evaluation has not passed review, so it cannot reach production.
    skills
        .promote(&admin(TENANT), &version, SkillStatus::Candidate)
        .await
        .expect("draft → candidate");
    skills
        .promote(&admin(TENANT), &version, SkillStatus::Evaluating)
        .await
        .expect("candidate → evaluating");
    let error = skills
        .promote(&admin(TENANT), &version, SkillStatus::Active)
        .await
        .expect_err("evaluating must pass through approved");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    // Nothing the refusal touched moved, and no event was staged.
    assert!(
        skills
            .active_versions(TENANT)
            .await
            .expect("read view")
            .is_empty(),
        "no unapproved version is in production"
    );
    let after = event_types(&fixture.pool, TENANT).await;
    assert_eq!(
        after.len(),
        before.len() + 2,
        "only the two legal promotions were recorded"
    );
    assert_eq!(
        after
            .iter()
            .filter(|kind| *kind == "skill.version_promoted")
            .count(),
        2,
        "a refused promotion writes no event"
    );

    let versions = versions_of(skills, TENANT, &skill).await;
    assert_eq!(versions, vec![SkillStatus::Evaluating]);

    drop_pool(&fixture.pool, &fixture.name).await;
}

/// The version states of one skill, read through the store's own view of the ladder.
async fn versions_of(skills: &SkillStore, tenant: &str, skill: &CanonicalId) -> Vec<SkillStatus> {
    let mut tx = skills.pool().begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, tenant)
        .await
        .expect("tenant context");
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT status FROM skill_versions WHERE skill_id = $1 ORDER BY semver")
            .bind(skill.to_string())
            .fetch_all(&mut *tx)
            .await
            .expect("read statuses");
    tx.commit().await.expect("commit");
    rows.into_iter()
        .map(|value| SkillStatus::parse(&value).expect("a §11.5 state"))
        .collect()
}

#[tokio::test]
async fn promotion_follows_the_ladder_and_only_active_resolves() {
    let Some(fixture) = prepare("skills_ladder").await else {
        return blocked_marker();
    };
    let skills = &fixture.skills;
    let skill = new_skill(skills, TENANT, "release-runbook").await;
    let version = new_version(skills, TENANT, &skill, "1.0.0").await;

    skills
        .promote(&admin(TENANT), &version, SkillStatus::Candidate)
        .await
        .expect("draft → candidate");
    skills
        .promote(&admin(TENANT), &version, SkillStatus::Evaluating)
        .await
        .expect("candidate → evaluating");
    let approved = skills
        .promote(&admin(TENANT), &version, SkillStatus::Approved)
        .await
        .expect("evaluating → approved");
    assert_eq!(approved.version.status, SkillStatus::Approved);
    assert!(
        !approved.version.resolves(),
        "an approval that was never promoted is not production"
    );
    assert!(
        skills
            .active_versions(TENANT)
            .await
            .expect("read view")
            .is_empty(),
        "APPROVED is excluded with the rule that excluded it"
    );

    let active = skills
        .promote(&admin(TENANT), &version, SkillStatus::Active)
        .await
        .expect("approved → active");
    assert_eq!(active.version.status, SkillStatus::Active);
    assert!(active.version.resolves());
    assert_eq!(active.previous_active_version_id, None);

    let view = skills.active_versions(TENANT).await.expect("read view");
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].1.id, version);
    assert_eq!(view[0].1.status, SkillStatus::Active);
    assert_eq!(view[0].0.current_active_version_id, Some(version));

    // Leaving ACTIVE is the ladder's own edge and takes the version out of production.
    let deprecated = skills
        .promote(&admin(TENANT), &version, SkillStatus::Deprecated)
        .await
        .expect("active → deprecated");
    assert_eq!(deprecated.previous_active_version_id, Some(version));
    assert!(
        skills
            .active_versions(TENANT)
            .await
            .expect("read view")
            .is_empty(),
        "a deprecated version no longer resolves"
    );
    skills
        .promote(&admin(TENANT), &version, SkillStatus::Retired)
        .await
        .expect("deprecated → retired");
    let error = skills
        .promote(&admin(TENANT), &version, SkillStatus::Active)
        .await
        .expect_err("a retired version cannot be revived");
    assert_eq!(error.code(), "RUNTIME_ILLEGAL_TRANSITION");

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn a_skill_has_at_most_one_active_version() {
    let Some(fixture) = prepare("skills_single_active").await else {
        return blocked_marker();
    };
    let skills = &fixture.skills;
    let skill = new_skill(skills, TENANT, "release-runbook").await;
    let first = new_version(skills, TENANT, &skill, "1.0.0").await;
    let second = new_version(skills, TENANT, &skill, "2.0.0").await;
    walk_to(skills, TENANT, &first, SkillStatus::Active)
        .await
        .expect("first version to active");
    walk_to(skills, TENANT, &second, SkillStatus::Approved)
        .await
        .expect("second version to approved");

    let error = skills
        .promote(&admin(TENANT), &second, SkillStatus::Active)
        .await
        .expect_err("two active versions would be ambiguous");
    assert_eq!(error.code(), "CONFLICT_STATE");
    assert!(
        error.to_string().contains(&first.to_string()),
        "the refusal names the version that is still active"
    );

    // The refused promotion changed nothing.
    let view = skills.active_versions(TENANT).await.expect("read view");
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].1.id, first);
    assert_eq!(
        versions_of(skills, TENANT, &skill).await,
        vec![SkillStatus::Active, SkillStatus::Approved]
    );

    // Deprecating the first is the ladder's own step, and then the second may be promoted.
    skills
        .promote(&admin(TENANT), &first, SkillStatus::Deprecated)
        .await
        .expect("deprecate the first");
    let promoted = skills
        .promote(&admin(TENANT), &second, SkillStatus::Active)
        .await
        .expect("second → active");
    assert_eq!(
        promoted.previous_active_version_id, None,
        "the pointer was cleared"
    );
    let view = skills.active_versions(TENANT).await.expect("read view");
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].1.id, second);
    assert_eq!(view[0].0.current_active_version_id, Some(second));

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn the_active_view_is_tenant_scoped_and_ordered() {
    let Some(fixture) = prepare("skills_view").await else {
        return blocked_marker();
    };
    let skills = &fixture.skills;

    for (tenant, name) in [(TENANT, "beta"), (TENANT, "alpha"), (TENANT_B, "gamma")] {
        let skill = new_skill(skills, tenant, name).await;
        let version = new_version(skills, tenant, &skill, "1.0.0").await;
        walk_to(skills, tenant, &version, SkillStatus::Active)
            .await
            .expect("promote to active");
    }

    let view = skills.active_versions(TENANT).await.expect("read view");
    let names: Vec<&str> = view.iter().map(|(skill, _)| skill.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha", "beta"],
        "one tenant's view only, ordered by name"
    );
    assert_eq!(
        skills
            .active_versions(TENANT_B)
            .await
            .expect("read view")
            .len(),
        1,
        "the other tenant sees its own skill"
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}

#[tokio::test]
async fn an_unknown_skill_or_version_is_refused_without_writing() {
    let Some(fixture) = prepare("skills_unknown").await else {
        return blocked_marker();
    };
    let skills = &fixture.skills;
    let stranger = CanonicalId::generate(Prefix::Skill, &mut UlidGenerator::new());
    let error = skills
        .create_version(
            &admin(TENANT),
            &stranger,
            NewSkillVersion {
                semver: "1.0.0".to_string(),
                manifest: manifest(&[]),
                provenance: None,
            },
        )
        .await
        .expect_err("no such skill");
    assert_eq!(error.code(), "NOT_FOUND");

    let missing_version = CanonicalId::generate(Prefix::SkillVersion, &mut UlidGenerator::new());
    let error = skills
        .promote(&admin(TENANT), &missing_version, SkillStatus::Candidate)
        .await
        .expect_err("no such version");
    assert_eq!(error.code(), "NOT_FOUND");

    // A skill of another tenant is not visible at all.
    let other = new_skill(skills, TENANT_B, "release-runbook").await;
    let error = skills
        .create_version(
            &admin(TENANT),
            &other,
            NewSkillVersion {
                semver: "1.0.0".to_string(),
                manifest: manifest(&[]),
                provenance: None,
            },
        )
        .await
        .expect_err("another tenant's skill is out of scope");
    assert_eq!(error.code(), "NOT_FOUND");

    assert!(
        event_types(&fixture.pool, TENANT).await.is_empty(),
        "a refusal writes no event and no row"
    );

    drop_pool(&fixture.pool, &fixture.name).await;
}
