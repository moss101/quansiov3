//! Policy, RBAC, privacy guards and approval verification tests (RUN-006).
//!
//! These tests exercise DOMAIN.md §7.1, §7.3 and §12 through
//! [`quansio_server::policy`] against a real scratch PostgreSQL database: the fixed
//! evaluation precedence, tier-4 `always` rejection, approval substitution and
//! single-use replay resistance, supersession on changed parameters, expiry, content-
//! trust escalation with an untrusted-origin preview, server-signature verification and
//! the Run `WAITING_APPROVAL` park/resume integration.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return. The approval signing key is supplied directly
//! to [`ApprovalSigner::new`]; the production key comes from
//! `QUANSIO_APPROVAL_SIGNING_KEY` via [`ApprovalSigner::from_env`].

use chrono::{Duration, Utc};
use quansio_capability::{Decision, EffectClass, ResourceSelector, Tier, UserRuleDecision};
use quansio_core::{CanonicalId, CorrelationId, Digest, Generation, Prefix, UlidGenerator};
use quansio_events::{EventStore, RuntimeEvent};
use sqlx::{PgPool, Row};

use quansio_server::control::schema;
use quansio_server::policy::{
    ActionFamily, ActorRoles, Amount, ApprovalFailure, ApprovalRuntime, ApprovalSigner,
    ConsequencePreview, DataClass, DispatchBinding, EgressDestination, EgressGrantSet,
    EvaluationRequest, PolicyError, PolicyEvaluator, PolicyReason, PolicySet, RequestedOf,
    SequenceContext, SequenceGuard, TenantRole, TrustLevel, UntrustedOrigin, UserRule,
    WorkspaceRole,
};
use quansio_server::runtime::state_machine::{
    NewRun, Run, RunStatus, RunTriggerKind, RuntimeEngine, RuntimeIdentity,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0RRRRR";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0RRRRR";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0RRRRR";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0RRRRR";
const EFFECT: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0RRRRR";
const EFFECT_B: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0SSSSS";

/// The server key used by the tests; the production key comes from configuration.
const TEST_KEY: &[u8] = b"test-server-approval-key";

struct Fixture {
    name: String,
    url: String,
    pool: PgPool,
    runtime: RuntimeEngine,
    approvals: ApprovalRuntime,
    agent_thread: CanonicalId,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;

    let mut generator = UlidGenerator::new();
    let agent_thread = CanonicalId::generate(Prefix::AgentThread, &mut generator);
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant context");
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, generation, status) \
         VALUES ($1, $2, $3, 'teammate', 1, 'ACTIVE')",
    )
    .bind(agent_thread.to_string())
    .bind(TENANT)
    .bind(WORKSPACE)
    .execute(&mut *tx)
    .await
    .expect("agent thread");
    tx.commit().await.expect("commit");

    let identity = RuntimeIdentity::system(
        TENANT,
        "policy-test",
        CorrelationId::generate(&mut generator),
    );
    let runtime = RuntimeEngine::new(pool.clone(), identity.clone()).expect("runtime");
    let approvals = ApprovalRuntime::new(pool.clone(), identity).expect("approval runtime");
    Some(Fixture {
        url: scratch_url(&name),
        name,
        pool,
        runtime,
        approvals,
        agent_thread,
    })
}

fn scratch_url(name: &str) -> String {
    common::scratch_url(name)
}

fn node_id() -> CanonicalId {
    CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("work node id")
}

async fn started_run(fixture: &Fixture) -> Run {
    let run = fixture
        .runtime
        .create_run(NewRun::new(
            WORKSPACE,
            node_id(),
            fixture.agent_thread,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("create run");
    let run = fixture
        .runtime
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue");
    fixture
        .runtime
        .start(&run.id, run.generation)
        .await
        .expect("start")
}

async fn all_events(fixture: &Fixture) -> Vec<RuntimeEvent> {
    EventStore::new(fixture.pool.clone())
        .read_events_after(TENANT, None, 1_000)
        .await
        .expect("events")
}

async fn approval_events(fixture: &Fixture) -> Vec<RuntimeEvent> {
    all_events(fixture)
        .await
        .into_iter()
        .filter(|event| event.event_type.family().as_str() == "approval")
        .collect()
}

async fn count_execute(pool: &PgPool, sql: &str) -> i64 {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    let value: i64 = sqlx::query_scalar(sql)
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    tx.commit().await.expect("commit");
    value
}

/// Insert one tenant- or workspace-scope policy row carrying `rules`.
async fn insert_policy(fixture: &Fixture, id: &str, scope: &str, rules: serde_json::Value) {
    let workspace: Option<&str> = if scope == "workspace" {
        Some(WORKSPACE)
    } else {
        None
    };
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(TENANT)
    .bind(workspace)
    .bind(scope)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .expect("insert policy");
    tx.commit().await.expect("commit");
}

/// Insert one `PROPOSED` EffectRecord for the approval tests.
#[allow(clippy::too_many_arguments)]
async fn insert_effect(
    fixture: &Fixture,
    run_id: &str,
    effect_id: &str,
    effect_class: &str,
    tier: i16,
    params: &Digest,
    generation: i64,
) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    sqlx::query(
        "INSERT INTO effect_records (id, tenant_id, workspace_id, run_id, effect_class, tier, \
         resource, params_digest, idempotency_key, capability_projection_id, status, generation) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'cap_x', 'PROPOSED', $10)",
    )
    .bind(effect_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(run_id)
    .bind(effect_class)
    .bind(tier)
    .bind(serde_json::json!({"kind": "domain", "selector": "api.example.com"}))
    .bind(params.as_str())
    .bind(format!("idem-{effect_id}"))
    .bind(generation)
    .execute(&mut *tx)
    .await
    .expect("insert effect");
    tx.commit().await.expect("commit");
}

async fn effect_state(fixture: &Fixture, effect_id: &str) -> (String, Option<String>) {
    let mut tx = fixture.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    let row = sqlx::query("SELECT status, approval_receipt_id FROM effect_records WHERE id = $1")
        .bind(effect_id)
        .fetch_one(&mut *tx)
        .await
        .expect("effect row");
    tx.commit().await.expect("commit");
    (row.get("status"), row.get("approval_receipt_id"))
}

fn request_for(
    run_id: &str,
    effect_id: &str,
    params: &Digest,
    preview: ConsequencePreview,
    expires_at: chrono::DateTime<Utc>,
) -> quansio_server::policy::NewApprovalRequest {
    quansio_server::policy::NewApprovalRequest::new(
        run_id,
        WORKSPACE,
        effect_id,
        vec![RequestedOf::User(USER.to_string())],
        "send the message",
        preview,
        params.clone(),
        "cap_x",
        expires_at,
    )
}

fn preview() -> ConsequencePreview {
    ConsequencePreview::new(
        vec!["ops@example.com".to_string()],
        vec![Amount::new("USD", 0)],
        vec!["api.example.com".to_string()],
        Vec::new(),
        None,
    )
}

fn signer() -> ApprovalSigner {
    ApprovalSigner::new(TEST_KEY.to_vec()).expect("signer")
}

/// Build and grant a request for one exact effect binding, returning the receipt id.
async fn granted_receipt(
    fixture: &Fixture,
    run_id: &str,
    effect_id: &str,
    params: &Digest,
    generation: Generation,
) -> String {
    let record = fixture
        .approvals
        .store()
        .create_approval_request(&request_for(
            run_id,
            effect_id,
            params,
            preview(),
            Utc::now() + Duration::seconds(600),
        ))
        .await
        .expect("create request");
    let receipt = fixture
        .approvals
        .store()
        .grant_approval(
            &record.id.to_string(),
            USER,
            generation,
            &signer(),
            Utc::now(),
        )
        .await
        .expect("grant");
    receipt.id.to_string()
}

fn class(value: &str) -> EffectClass {
    EffectClass::parse(value).expect("effect class")
}

fn selector(value: &str) -> ResourceSelector {
    ResourceSelector::from_parts("domain", value, &[]).expect("selector")
}

#[tokio::test]
async fn policy_deny_wins_over_user_always_and_a_missing_rule_denies() {
    let Some(f) = prepare("policy_deny").await else {
        blocked_marker();
        return;
    };
    insert_policy(
        &f,
        "pol_01J8Z3K6F1N8VQ2X5W9Y0RRRRR",
        "tenant",
        serde_json::json!([{
            "effect_class": "message.send",
            "resource_selector": {"kind": "domain", "selector": "*"},
            "decision": "deny"
        }]),
    )
    .await;

    let rule_id = "rule_01J8Z3K6F1N8VQ2X5W9Y0RRRRR";
    let user_rule = UserRule {
        id: rule_id.to_string(),
        user_id: USER.to_string(),
        workspace_id: WORKSPACE.to_string(),
        effect_class: class("message.send"),
        resource_selector: selector("*"),
        decision: UserRuleDecision::Always,
        expires_at: None,
    };
    f.approvals
        .store()
        .store_user_rule(&user_rule, Tier::new(3).expect("tier"))
        .await
        .expect("store user rule");

    let policies = f
        .approvals
        .store()
        .load_policy_set(WORKSPACE)
        .await
        .expect("policy set");
    let rules = f
        .approvals
        .store()
        .load_user_rules(WORKSPACE, USER)
        .await
        .expect("user rules");
    assert_eq!(rules.len(), 1);

    let effect = class("message.send");
    let resource = selector("api.example.com");
    let grants = EgressGrantSet::empty();
    let sequence = SequenceContext::empty();
    let outcome = PolicyEvaluator::new(&policies, &rules).evaluate(&EvaluationRequest {
        effect_class: &effect,
        resource: &resource,
        catalog_tier: Tier::new(3).expect("tier"),
        action: ActionFamily::ExecuteEffect { tier: 3 },
        roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
        derived_from_trust: Some(TrustLevel::TrustedUser),
        data_classes: &[],
        destination: None,
        egress_grants: &grants,
        capability_projection_id: Some("cap_x"),
        sequence_guards: &[],
        sequence: &sequence,
        user_id: Some(USER),
        now: Utc::now(),
    });
    assert!(outcome.is_deny(), "{outcome:?}");
    assert_eq!(outcome.reason, PolicyReason::PolicyExplicitDeny);

    // A class with no rule at tier >= 1 has no authority: it denies.
    let missing = class("record.delete");
    let outcome = PolicyEvaluator::new(&policies, &rules).evaluate(&EvaluationRequest {
        effect_class: &missing,
        resource: &resource,
        catalog_tier: Tier::new(3).expect("tier"),
        action: ActionFamily::ExecuteEffect { tier: 3 },
        roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
        derived_from_trust: Some(TrustLevel::TrustedUser),
        data_classes: &[],
        destination: None,
        egress_grants: &grants,
        capability_projection_id: Some("cap_x"),
        sequence_guards: &[],
        sequence: &sequence,
        user_id: Some(USER),
        now: Utc::now(),
    });
    assert!(outcome.is_deny());
    assert_eq!(
        outcome.reason,
        PolicyReason::NoMatchingPolicyRule { tier: 3 }
    );
    drop_pool(&f.pool, &f.name).await;
}

#[test]
fn protected_egress_without_a_grant_is_denied_and_settlement_before_reservation_is_refused() {
    let policies = PolicySet::default();
    let evaluator = PolicyEvaluator::new(&policies, &[]);
    let effect = class("data.upload.protected");
    let resource = selector("api.example.com");
    let destination = EgressDestination::domain("api.example.com");
    let classes = [DataClass::parse("pii").expect("class")];
    let grants = EgressGrantSet::empty();
    let sequence = SequenceContext::empty();
    let outcome = evaluator.evaluate(&EvaluationRequest {
        effect_class: &effect,
        resource: &resource,
        catalog_tier: Tier::new(4).expect("tier"),
        action: ActionFamily::ExecuteEffect { tier: 4 },
        roles: ActorRoles::new(Some(TenantRole::Admin), Some(WorkspaceRole::Admin)),
        derived_from_trust: Some(TrustLevel::TrustedUser),
        data_classes: &classes,
        destination: Some(&destination),
        egress_grants: &grants,
        capability_projection_id: Some("cap_x"),
        sequence_guards: &[],
        sequence: &sequence,
        user_id: Some(USER),
        now: Utc::now(),
    });
    assert!(outcome.is_deny());
    assert_eq!(outcome.reason.as_str(), "privacy_protected_egress_denied");

    // A settlement that precedes its reservation is refused.
    let settle = class("record.update");
    let settlement = SequenceContext {
        settled_effect_ids: vec![EFFECT.to_string()],
        ..SequenceContext::empty()
    };
    let guards = [SequenceGuard::ReservationBeforeSettlement];
    let outcome = evaluator.evaluate(&EvaluationRequest {
        effect_class: &settle,
        resource: &resource,
        catalog_tier: Tier::new(2).expect("tier"),
        action: ActionFamily::ExecuteEffect { tier: 2 },
        roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
        derived_from_trust: Some(TrustLevel::TrustedUser),
        data_classes: &[],
        destination: None,
        egress_grants: &grants,
        capability_projection_id: Some("cap_x"),
        sequence_guards: &guards,
        sequence: &settlement,
        user_id: Some(USER),
        now: Utc::now(),
    });
    assert!(outcome.is_deny());
    assert_eq!(
        outcome.reason.as_str(),
        "sequence_settlement_before_reservation"
    );

    // An approval used before it was granted is refused.
    let usage = SequenceContext {
        granted_approval_ids: vec!["apr_a".to_string()],
        used_approval_ids: vec!["apr_b".to_string()],
        ..SequenceContext::empty()
    };
    let approval_guards = [SequenceGuard::ApprovalGrantedBeforeUse];
    let outcome = evaluator.evaluate(&EvaluationRequest {
        effect_class: &settle,
        resource: &resource,
        catalog_tier: Tier::new(2).expect("tier"),
        action: ActionFamily::ExecuteEffect { tier: 2 },
        roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
        derived_from_trust: Some(TrustLevel::TrustedUser),
        data_classes: &[],
        destination: None,
        egress_grants: &grants,
        capability_projection_id: Some("cap_x"),
        sequence_guards: &approval_guards,
        sequence: &usage,
        user_id: Some(USER),
        now: Utc::now(),
    });
    assert!(outcome.is_deny());
    assert_eq!(outcome.reason.as_str(), "sequence_approval_not_granted");
}

#[tokio::test]
async fn a_tier_four_always_user_rule_is_rejected_and_never_stored() {
    let Some(f) = prepare("policy_tier4").await else {
        blocked_marker();
        return;
    };
    let rule = UserRule {
        id: "rule_01J8Z3K6F1N8VQ2X5W9Y0TTTTT".to_string(),
        user_id: USER.to_string(),
        workspace_id: WORKSPACE.to_string(),
        effect_class: class("payment.execute"),
        resource_selector: selector("*"),
        decision: UserRuleDecision::Always,
        expires_at: None,
    };
    let error = f
        .approvals
        .store()
        .store_user_rule(&rule, Tier::new(4).expect("tier"))
        .await
        .expect_err("tier-4 always must be rejected");
    assert!(matches!(error, PolicyError::TierFourAlwaysRejected { .. }));
    assert_eq!(error.code(), "POLICY_DENIED");
    assert_eq!(
        count_execute(&f.pool, "SELECT count(*) FROM user_rules").await,
        0,
        "an illegal rule must not be stored as effective"
    );
    assert!(approval_events(&f).await.is_empty());
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn approval_substitution_fails_closed_and_writes_nothing() {
    let Some(f) = prepare("policy_substitution").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&f).await;
    let run_id = run.id.to_string();
    let generation = run.generation;
    let params = Digest::of(b"params-a");
    insert_effect(
        &f,
        &run_id,
        EFFECT,
        "message.send",
        3,
        &params,
        generation.get() as i64,
    )
    .await;
    let receipt_id = granted_receipt(&f, &run_id, EFFECT, &params, generation).await;
    let signer = signer();

    // Different parameters.
    let changed = Digest::of(b"params-b");
    let error = f
        .approvals
        .store()
        .verify_and_consume_receipt(
            &receipt_id,
            &DispatchBinding {
                effect_id: EFFECT.to_string(),
                params_digest: changed,
                generation,
            },
            &signer,
            Utc::now(),
        )
        .await
        .expect_err("changed params");
    assert!(matches!(
        error,
        PolicyError::ApprovalInvalid(ApprovalFailure::ParamsChanged { .. })
    ));

    // Different effect.
    let error = f
        .approvals
        .store()
        .verify_and_consume_receipt(
            &receipt_id,
            &DispatchBinding {
                effect_id: EFFECT_B.to_string(),
                params_digest: params.clone(),
                generation,
            },
            &signer,
            Utc::now(),
        )
        .await
        .expect_err("changed effect");
    assert!(matches!(
        error,
        PolicyError::ApprovalInvalid(ApprovalFailure::EffectMismatch { .. })
    ));

    // Different generation.
    let error = f
        .approvals
        .store()
        .verify_and_consume_receipt(
            &receipt_id,
            &DispatchBinding {
                effect_id: EFFECT.to_string(),
                params_digest: params.clone(),
                generation: Generation::new(2).expect("generation"),
            },
            &signer,
            Utc::now(),
        )
        .await
        .expect_err("stale generation");
    assert!(matches!(
        error,
        PolicyError::ApprovalInvalid(ApprovalFailure::GenerationStale { .. })
    ));

    assert_eq!(effect_state(&f, EFFECT).await.0, "PROPOSED");
    assert_eq!(
        approval_events(&f).await.len(),
        2,
        "only requested + granted"
    );

    // The exact binding is consumed once, then single-use replay fails closed.
    let binding = DispatchBinding {
        effect_id: EFFECT.to_string(),
        params_digest: params.clone(),
        generation,
    };
    f.approvals
        .store()
        .verify_and_consume_receipt(&receipt_id, &binding, &signer, Utc::now())
        .await
        .expect("consume");
    assert_eq!(effect_state(&f, EFFECT).await.0, "AUTHORIZED");
    let events_after_consume = approval_events(&f).await;
    assert_eq!(events_after_consume.len(), 3);

    let replay = f
        .approvals
        .store()
        .verify_and_consume_receipt(&receipt_id, &binding, &signer, Utc::now())
        .await
        .expect_err("single-use replay");
    assert!(matches!(
        replay,
        PolicyError::ApprovalInvalid(ApprovalFailure::AlreadyUsed)
    ));
    assert_eq!(approval_events(&f).await.len(), events_after_consume.len());
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_tampered_receipt_fails_verification_and_writes_nothing() {
    let Some(f) = prepare("policy_tamper").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&f).await;
    let run_id = run.id.to_string();
    let generation = run.generation;
    let params = Digest::of(b"params-a");
    insert_effect(
        &f,
        &run_id,
        EFFECT,
        "message.send",
        3,
        &params,
        generation.get() as i64,
    )
    .await;
    let receipt_id = granted_receipt(&f, &run_id, EFFECT, &params, generation).await;

    let mut tx = f.pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    sqlx::query("UPDATE approval_receipts SET signature = $1 WHERE id = $2")
        .bind(hex::encode([7u8; 32]))
        .bind(&receipt_id)
        .execute(&mut *tx)
        .await
        .expect("tamper");
    tx.commit().await.expect("commit");

    let before = approval_events(&f).await.len();
    let error = f
        .approvals
        .store()
        .verify_and_consume_receipt(
            &receipt_id,
            &DispatchBinding {
                effect_id: EFFECT.to_string(),
                params_digest: params,
                generation,
            },
            &signer(),
            Utc::now(),
        )
        .await
        .expect_err("tampered receipt");
    assert!(matches!(
        error,
        PolicyError::ApprovalInvalid(ApprovalFailure::SignatureInvalid)
    ));
    assert_eq!(effect_state(&f, EFFECT).await.0, "PROPOSED");
    assert_eq!(approval_events(&f).await.len(), before);
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn expired_and_superseded_requests_cannot_be_granted_and_changes_supersede() {
    let Some(f) = prepare("policy_supersede").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&f).await;
    let run_id = run.id.to_string();
    let generation = run.generation;
    let params = Digest::of(b"params-a");
    insert_effect(
        &f,
        &run_id,
        EFFECT_B,
        "message.send",
        3,
        &params,
        generation.get() as i64,
    )
    .await;

    // An expired request is never granted and writes no receipt.
    let expired = f
        .approvals
        .store()
        .create_approval_request(&request_for(
            &run_id,
            EFFECT_B,
            &params,
            preview(),
            Utc::now() - Duration::seconds(1),
        ))
        .await
        .expect("expired request");
    let error = f
        .approvals
        .store()
        .grant_approval(
            &expired.id.to_string(),
            USER,
            generation,
            &signer(),
            Utc::now(),
        )
        .await
        .expect_err("expired");
    assert!(matches!(error, PolicyError::ApprovalNotPending { .. }));
    assert_eq!(
        count_execute(&f.pool, "SELECT count(*) FROM approval_receipts").await,
        0
    );

    // A changed parameter digest supersedes the pending request.
    let pending = f
        .approvals
        .store()
        .create_approval_request(&request_for(
            &run_id,
            EFFECT,
            &params,
            preview(),
            Utc::now() + Duration::seconds(600),
        ))
        .await
        .expect("pending request");
    // Same digest: a no-op, no event.
    let events_before_noop = approval_events(&f).await.len();
    assert_eq!(
        f.approvals
            .store()
            .supersede_if_params_changed(EFFECT, &params)
            .await
            .expect("no-op supersede"),
        None
    );
    assert_eq!(approval_events(&f).await.len(), events_before_noop);

    let changed = Digest::of(b"params-changed");
    let superseded = f
        .approvals
        .store()
        .supersede_if_params_changed(EFFECT, &changed)
        .await
        .expect("supersede");
    assert_eq!(superseded.as_deref(), Some(pending.id.to_string().as_str()));
    assert_eq!(approval_events(&f).await.len(), events_before_noop + 1);
    let loaded = f
        .approvals
        .store()
        .load_approval_request(&pending.id.to_string())
        .await
        .expect("load");
    assert_eq!(
        loaded.status,
        quansio_server::policy::ApprovalRequestStatus::Superseded
    );
    let error = f
        .approvals
        .store()
        .grant_approval(
            &pending.id.to_string(),
            USER,
            generation,
            &signer(),
            Utc::now(),
        )
        .await
        .expect_err("superseded");
    assert!(matches!(error, PolicyError::ApprovalNotPending { .. }));
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn escalation_forbids_always_and_the_preview_carries_the_untrusted_origin() {
    let Some(f) = prepare("policy_escalation").await else {
        blocked_marker();
        return;
    };
    insert_policy(
        &f,
        "pol_01J8Z3K6F1N8VQ2X5W9Y0SSSSS",
        "tenant",
        serde_json::json!([{
            "effect_class": "message.send",
            "resource_selector": {"kind": "domain", "selector": "*"},
            "decision": "allow"
        }]),
    )
    .await;
    let user_rule = UserRule {
        id: "rule_01J8Z3K6F1N8VQ2X5W9Y0SSSSS".to_string(),
        user_id: USER.to_string(),
        workspace_id: WORKSPACE.to_string(),
        effect_class: class("message.send"),
        resource_selector: selector("*"),
        decision: UserRuleDecision::Always,
        expires_at: None,
    };
    f.approvals
        .store()
        .store_user_rule(&user_rule, Tier::new(3).expect("tier"))
        .await
        .expect("store rule");

    let policies = f
        .approvals
        .store()
        .load_policy_set(WORKSPACE)
        .await
        .expect("policy set");
    let rules = f
        .approvals
        .store()
        .load_user_rules(WORKSPACE, USER)
        .await
        .expect("user rules");
    let effect = class("message.send");
    let resource = selector("api.example.com");
    let grants = EgressGrantSet::empty();
    let sequence = SequenceContext::empty();
    let outcome = PolicyEvaluator::new(&policies, &rules).evaluate(&EvaluationRequest {
        effect_class: &effect,
        resource: &resource,
        catalog_tier: Tier::new(2).expect("tier"),
        action: ActionFamily::ExecuteEffect { tier: 2 },
        roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
        derived_from_trust: Some(TrustLevel::UntrustedExternal),
        data_classes: &[],
        destination: None,
        egress_grants: &grants,
        capability_projection_id: Some("cap_x"),
        sequence_guards: &[],
        sequence: &sequence,
        user_id: Some(USER),
        now: Utc::now(),
    });
    assert!(outcome.escalated);
    assert_eq!(outcome.effective_tier.get(), 3);
    assert!(outcome.requires_approval());
    assert!(!outcome.standing_allow);
    assert_eq!(
        outcome.rejected_user_rules[0].reason.as_str(),
        "user_rule_escalated_always_rejected"
    );

    // The request preview records the untrusted origin for the human to see.
    let run = started_run(&f).await;
    let run_id = run.id.to_string();
    let params = Digest::of(b"params-a");
    insert_effect(
        &f,
        &run_id,
        EFFECT,
        "message.send",
        3,
        &params,
        run.generation.get() as i64,
    )
    .await;
    let mut escalated_preview = preview();
    escalated_preview.untrusted_origin = Some(UntrustedOrigin::new(vec!["seg_web".to_string()]));
    let record = f
        .approvals
        .store()
        .create_approval_request(&request_for(
            &run_id,
            EFFECT,
            &params,
            escalated_preview,
            Utc::now() + Duration::seconds(600),
        ))
        .await
        .expect("escalated request");
    assert!(record.consequence_preview.shows_untrusted_origin());
    assert_eq!(
        record
            .consequence_preview
            .untrusted_origin
            .as_ref()
            .expect("origin")
            .trust(),
        TrustLevel::UntrustedExternal
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn exactly_one_approval_event_per_transition_and_denial_changes_no_effect_state() {
    let Some(f) = prepare("policy_events").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&f).await;
    let run_id = run.id.to_string();
    let generation = run.generation;
    let params = Digest::of(b"params-a");
    insert_effect(
        &f,
        &run_id,
        EFFECT,
        "message.send",
        3,
        &params,
        generation.get() as i64,
    )
    .await;

    let mut count = approval_events(&f).await.len();
    let record = f
        .approvals
        .store()
        .create_approval_request(&request_for(
            &run_id,
            EFFECT,
            &params,
            preview(),
            Utc::now() + Duration::seconds(600),
        ))
        .await
        .expect("request");
    assert_eq!(approval_events(&f).await.len(), count + 1);
    count += 1;
    assert_eq!(
        approval_events(&f)
            .await
            .last()
            .expect("event")
            .event_type
            .to_string(),
        "approval.requested"
    );

    // A denial emits exactly one event and leaves the effect untouched.
    f.approvals
        .store()
        .deny_approval(&record.id.to_string())
        .await
        .expect("deny");
    let events = approval_events(&f).await;
    assert_eq!(events.len(), count + 1);
    assert_eq!(
        events.last().expect("event").event_type.to_string(),
        "approval.denied"
    );
    let (status, receipt) = effect_state(&f, EFFECT).await;
    assert_eq!(status, "PROPOSED");
    assert!(receipt.is_none());
    count += 1;

    // Granting a fresh request emits exactly one approval.granted.
    let second = f
        .approvals
        .store()
        .create_approval_request(&request_for(
            &run_id,
            EFFECT,
            &params,
            preview(),
            Utc::now() + Duration::seconds(600),
        ))
        .await
        .expect("second request");
    count += 1;
    let receipt = f
        .approvals
        .store()
        .grant_approval(
            &second.id.to_string(),
            USER,
            generation,
            &signer(),
            Utc::now(),
        )
        .await
        .expect("grant");
    let events = approval_events(&f).await;
    assert_eq!(events.len(), count + 1);
    assert_eq!(
        events.last().expect("event").event_type.to_string(),
        "approval.granted"
    );
    count += 1;

    // Consuming the receipt emits exactly one approval.consumed.
    f.approvals
        .store()
        .verify_and_consume_receipt(
            &receipt.id.to_string(),
            &DispatchBinding {
                effect_id: EFFECT.to_string(),
                params_digest: params,
                generation,
            },
            &signer(),
            Utc::now(),
        )
        .await
        .expect("consume");
    let events = approval_events(&f).await;
    assert_eq!(events.len(), count + 1);
    assert_eq!(
        events.last().expect("event").event_type.to_string(),
        "approval.consumed"
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn a_run_parks_in_waiting_approval_and_resumes_only_on_its_matching_receipt() {
    let Some(f) = prepare("policy_park").await else {
        blocked_marker();
        return;
    };
    let run = started_run(&f).await;
    let generation = run.generation;
    let run_id = run.id.to_string();
    let params = Digest::of(b"params-a");
    insert_effect(
        &f,
        &run_id,
        EFFECT,
        "message.send",
        3,
        &params,
        generation.get() as i64,
    )
    .await;

    let parked = f
        .approvals
        .park_for_approval(
            request_for(
                &run_id,
                EFFECT,
                &params,
                preview(),
                Utc::now() + Duration::seconds(600),
            ),
            generation,
        )
        .await
        .expect("park");
    assert_eq!(parked.run.status, RunStatus::WaitingApproval);
    assert_eq!(parked.request.effect_id, EFFECT);
    let state = f
        .runtime
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state row");
    assert_eq!(
        state.pending_approvals,
        vec![parked.request.id.to_string()],
        "the request is recorded in protocol state"
    );

    let receipt = f
        .approvals
        .grant_and_resume(
            &parked.request.id.to_string(),
            USER,
            generation,
            &signer(),
            Utc::now(),
        )
        .await
        .expect("grant and resume");
    let resumed = f.runtime.store().load_run(&run.id).await.expect("run");
    assert_eq!(resumed.status, RunStatus::Running);
    let state = f
        .runtime
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state row");
    assert!(state.pending_approvals.is_empty());

    // The granted receipt authorizes exactly the effect it was requested for.
    f.approvals
        .store()
        .verify_and_consume_receipt(
            &receipt.id.to_string(),
            &DispatchBinding {
                effect_id: EFFECT.to_string(),
                params_digest: params,
                generation,
            },
            &signer(),
            Utc::now(),
        )
        .await
        .expect("consume after resume");
    drop_pool(&f.pool, &f.name).await;
}

#[test]
fn default_policy_evaluation_is_fail_closed_for_missing_inputs() {
    let policies = PolicySet::default();
    let evaluator = PolicyEvaluator::new(&policies, &[]);
    let effect = class("record.update");
    let resource = selector("api.example.com");
    let grants = EgressGrantSet::empty();
    let sequence = SequenceContext::empty();
    let outcome = evaluator.evaluate(&EvaluationRequest {
        effect_class: &effect,
        resource: &resource,
        catalog_tier: Tier::new(2).expect("tier"),
        action: ActionFamily::ExecuteEffect { tier: 2 },
        roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Editor)),
        // An absent trust derivation fails closed to UNTRUSTED_EXTERNAL and escalates.
        derived_from_trust: None,
        data_classes: &[],
        destination: None,
        egress_grants: &grants,
        capability_projection_id: None,
        sequence_guards: &[],
        sequence: &sequence,
        user_id: Some(USER),
        now: Utc::now(),
    });
    assert_eq!(outcome.decision, Decision::Deny);
    assert_eq!(outcome.reason, PolicyReason::CapabilityProjectionMissing);
}

// Keep `url` referenced so the reconnect handle stays available for future recovery
// assertions without a dead-code warning.
#[allow(dead_code)]
fn connection_url(fixture: &Fixture) -> &str {
    &fixture.url
}
