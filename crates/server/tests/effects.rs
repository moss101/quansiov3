//! Universal Effect Ledger tests (RUN-007, DOMAIN.md §7).
//!
//! These tests drive [`quansio_server::effects::EffectLedger`] against a real scratch
//! PostgreSQL database: reservation idempotency under concurrent dispatchers, dispatch
//! tokens, terminal settlement with exactly one `effect.*` event per transition, unknown
//! outcome reconciliation by class strategy (with blind retries refused), retry that
//! creates a new identity with the same idempotency key, the runtime protocol-state
//! integration that keeps `next_safe_action` on `ReconcileEffect` until settlement, and
//! tenant isolation.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` (see `tests/common/mod.rs`). Absent →
//! `BLOCKED_EXTERNAL` marker and return.

use quansio_capability::EffectClass;
use quansio_core::{CanonicalId, CorrelationId, Digest, Generation, Prefix, UlidGenerator};
use sqlx::PgPool;

use quansio_server::control::schema;
use quansio_server::effects::{
    EffectError, EffectLedger, EffectRecord, EffectResource, EffectStatus, EffectTarget, NewEffect,
    ReconciliationEvidence, ReconciliationStrategy, RetryAuthorization, TargetKind, ToolCallRef,
};
use quansio_server::runtime::protocol_state::{next_safe_action, NextAction};
use quansio_server::runtime::state_machine::{
    NewRun, Run, RunTriggerKind, RuntimeEngine, RuntimeIdentity,
};

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0EEEEE";

const TENANT_B: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const USER_B: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORKSPACE_B: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const WORK_NODE_B: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";

/// A `query`-strategy class (DOMAIN §7.1 `record.create`).
const QUERY_CLASS: &str = "record.create";
/// A `manual`-strategy class (DOMAIN §7.1 `process.exec.host`).
const MANUAL_CLASS: &str = "process.exec.host";

struct Fixture {
    name: String,
    pool: PgPool,
    engine: RuntimeEngine,
    ledger: EffectLedger,
    ledger_b: EffectLedger,
    agent_thread: CanonicalId,
}

async fn prepare(prefix: &str) -> Option<Fixture> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    seed_tenant(&pool, TENANT_B, USER_B, WORKSPACE_B, WORK_NODE_B).await;

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
        "effects-test",
        CorrelationId::generate(&mut generator),
    );
    let ledger = EffectLedger::new(pool.clone(), identity).expect("ledger");
    let identity_b = RuntimeIdentity::system(
        TENANT_B,
        "effects-test",
        CorrelationId::generate(&mut generator),
    );
    let ledger_b = EffectLedger::new(pool.clone(), identity_b).expect("ledger b");
    let engine = RuntimeEngine::new(
        pool.clone(),
        RuntimeIdentity::system(
            TENANT,
            "effects-test",
            CorrelationId::generate(&mut generator),
        ),
    )
    .expect("engine");
    Some(Fixture {
        name,
        pool,
        engine,
        ledger,
        ledger_b,
        agent_thread,
    })
}

/// A policy-authorized effect request with a stable idempotency key.
fn new_effect(ledger: &EffectLedger, workspace: &str, class: &str, resource: &str) -> NewEffect {
    let effect_class = EffectClass::parse(class).expect("effect class");
    let tier = ledger
        .taxonomy()
        .tier_for(&effect_class)
        .expect("registered tier");
    NewEffect::policy_authorized(
        workspace,
        effect_class,
        tier,
        EffectResource::new("domain", resource),
        Digest::of_canonical_json(&format!("{{\"resource\":\"{resource}\"}}")),
        "cap_run007",
        "pdc_run007",
        EffectTarget::new(TargetKind::Adapter, "connector_run007"),
        Generation::INITIAL,
    )
}

async fn reserve(ledger: &EffectLedger, effect: NewEffect) -> EffectRecord {
    ledger.reserve(effect).await.expect("reserve")
}

async fn dispatch(ledger: &EffectLedger, record: &EffectRecord) -> EffectRecord {
    ledger
        .mark_dispatched(
            &record.id,
            record.dispatch_token.as_deref().expect("dispatch token"),
        )
        .await
        .expect("dispatch")
}

async fn reserve_and_dispatch(ledger: &EffectLedger, effect: NewEffect) -> EffectRecord {
    let reserved = reserve(ledger, effect).await;
    dispatch(ledger, &reserved).await
}

async fn effect_event_types(ledger: &EffectLedger, effect_id: &str) -> Vec<String> {
    ledger
        .timeline(effect_id)
        .await
        .expect("timeline")
        .into_iter()
        .map(|event| event.event_type.to_string())
        .collect()
}

async fn running_run(fixture: &Fixture) -> Run {
    let run = fixture
        .engine
        .create_run(NewRun::new(
            WORKSPACE,
            CanonicalId::parse_typed(WORK_NODE, Prefix::WorkNode).expect("work node"),
            fixture.agent_thread,
            RunTriggerKind::Manual,
        ))
        .await
        .expect("create run");
    fixture
        .engine
        .enqueue(&run.id, run.generation)
        .await
        .expect("enqueue")
}

#[tokio::test]
async fn racing_dispatchers_reserve_exactly_one_action() {
    let Some(f) = prepare("effects_race").await else {
        blocked_marker();
        return;
    };
    let first = new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/racing");
    let second = first.clone();

    let (left, right) = tokio::join!(f.ledger.reserve(first), f.ledger.reserve(second));
    let outcomes = [left, right];
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "exactly one dispatcher may reserve the action"
    );
    let loser = outcomes
        .iter()
        .find_map(|outcome| outcome.as_ref().err())
        .expect("one racing dispatcher loses");
    assert!(
        matches!(loser, EffectError::DuplicateInFlight { .. }),
        "expected a duplicate refusal, got {loser:?}"
    );
    assert_eq!(loser.code(), "CONFLICT_IDEMPOTENCY_MISMATCH");
    let winner = outcomes
        .into_iter()
        .find_map(Result::ok)
        .expect("winning reservation");
    assert_eq!(winner.status, EffectStatus::Reserved);
    let found = f
        .ledger
        .find_by_key(&winner.effect_class, &winner.idempotency_key)
        .await
        .expect("key lookup");
    assert_eq!(found.len(), 1, "the partial unique index holds one row");

    // A duplicate while the first reservation is still in flight is refused too.
    let duplicate = new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/racing");
    let refused = f
        .ledger
        .reserve(duplicate)
        .await
        .expect_err("in-flight duplicate");
    assert!(matches!(refused, EffectError::DuplicateInFlight { .. }));
    assert_eq!(
        f.ledger
            .find_by_key(&winner.effect_class, &winner.idempotency_key)
            .await
            .expect("key lookup")
            .len(),
        1
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn settlement_is_terminal_with_one_event_per_transition() {
    let Some(f) = prepare("effects_settle").await else {
        blocked_marker();
        return;
    };
    let record = reserve_and_dispatch(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/settle"),
    )
    .await;
    let settled = f
        .ledger
        .settle_success(
            &record.id,
            Some("remote-1".to_string()),
            vec!["evd_1".into()],
        )
        .await
        .expect("settle");
    assert_eq!(settled.status, EffectStatus::SettledSuccess);
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.remote_ref.clone()),
        Some("remote-1".to_string())
    );

    let events = f.ledger.timeline(&record.id).await.expect("timeline");
    let types: Vec<String> = events
        .iter()
        .map(|event| event.event_type.to_string())
        .collect();
    assert_eq!(
        types,
        vec![
            "effect.proposed",
            "effect.authorized",
            "effect.reserved",
            "effect.dispatched",
            "effect.settled_success",
        ],
        "exactly one effect.* event per transition, in order"
    );
    let versions: Vec<u64> = events.iter().map(|event| event.aggregate_version).collect();
    assert_eq!(
        versions,
        vec![1, 2, 3, 4, 5],
        "aggregate versions are contiguous"
    );
    assert_eq!(
        types
            .iter()
            .filter(|event| event.as_str() == "effect.settled_success")
            .count(),
        1
    );

    let re_settled = f
        .ledger
        .settle_success(&record.id, None, Vec::new())
        .await
        .expect_err("re-settlement must be refused");
    assert!(matches!(re_settled, EffectError::AlreadySettled { .. }));
    assert_eq!(re_settled.code(), "CONFLICT_STATE");
    let reloaded = f.ledger.load(&record.id).await.expect("reload");
    assert_eq!(reloaded.status, EffectStatus::SettledSuccess);
    assert_eq!(
        f.ledger.timeline(&record.id).await.expect("timeline").len(),
        5,
        "a refused re-settlement writes no event"
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn unknown_outcomes_are_reconciled_by_strategy_and_never_retried() {
    let Some(f) = prepare("effects_unknown").await else {
        blocked_marker();
        return;
    };
    let record = reserve_and_dispatch(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/unknown"),
    )
    .await;
    let unknown = f
        .ledger
        .mark_outcome_unknown(&record.id, Some("timeout".to_string()))
        .await
        .expect("unknown");
    assert_eq!(unknown.status, EffectStatus::OutcomeUnknown);
    assert_eq!(
        f.ledger
            .list_unsettled()
            .await
            .expect("unsettled")
            .iter()
            .filter(|candidate| candidate.id == record.id)
            .count(),
        1
    );

    // A retry attempt on an unknown effect is refused and changes nothing.
    let refused = f
        .ledger
        .retry(&record.id, RetryAuthorization::RecordedDecision)
        .await
        .expect_err("unknown effects are never retried");
    assert!(matches!(refused, EffectError::RetryUnsafe { .. }));
    assert_eq!(refused.code(), "EFFECT_UNKNOWN_PENDING_RECONCILIATION");
    assert_eq!(
        f.ledger.load(&record.id).await.expect("reload").status,
        EffectStatus::OutcomeUnknown
    );
    assert_eq!(
        f.ledger
            .find_by_key(&record.effect_class, &record.idempotency_key)
            .await
            .expect("key lookup")
            .len(),
        1,
        "the refused retry created no record"
    );

    // Evidence that does not match the class strategy is refused.
    let mismatch = f
        .ledger
        .reconcile(
            &record.id,
            ReconciliationEvidence::Manual {
                evidence_ids: Vec::new(),
            },
        )
        .await
        .expect_err("a query class cannot be parked as manual");
    assert!(matches!(
        mismatch,
        EffectError::ReconciliationStrategyMismatch { .. }
    ));

    // The deterministic check settles the outcome; only reconciliation moved it.
    let reconciled = f
        .ledger
        .reconcile(
            &record.id,
            ReconciliationEvidence::Determined {
                landed: true,
                remote_ref: Some("remote-2".to_string()),
                evidence_ids: vec!["evd_2".into()],
            },
        )
        .await
        .expect("reconcile");
    assert_eq!(reconciled.status, EffectStatus::ReconciledSuccess);
    let types = effect_event_types(&f.ledger, &record.id).await;
    assert_eq!(
        &types[types.len() - 4..],
        &[
            "effect.dispatched",
            "effect.outcome_unknown",
            "effect.reconciling",
            "effect.reconciled_success",
        ]
    );
    let unsettled = f.ledger.list_unsettled().await.expect("unsettled");
    assert!(unsettled.iter().all(|candidate| candidate.id != record.id));
    let not_required = f
        .ledger
        .reconcile(
            &record.id,
            ReconciliationEvidence::Determined {
                landed: false,
                remote_ref: None,
                evidence_ids: Vec::new(),
            },
        )
        .await
        .expect_err("settled record needs no reconciliation");
    assert!(matches!(
        not_required,
        EffectError::ReconcileNotRequired { .. }
    ));
    let after = f
        .ledger
        .retry(&record.id, RetryAuthorization::RecordedDecision)
        .await
        .expect_err("a reconciled record is not retryable");
    assert!(matches!(after, EffectError::RetryNotAllowed { .. }));

    // A manual-strategy class parks in RECONCILIATION_MANUAL.
    let manual = reserve_and_dispatch(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, MANUAL_CLASS, "host/bin/deploy"),
    )
    .await;
    f.ledger
        .mark_outcome_unknown(&manual.id, Some("disconnect".to_string()))
        .await
        .expect("unknown");
    let mismatch = f
        .ledger
        .reconcile(
            &manual.id,
            ReconciliationEvidence::Determined {
                landed: true,
                remote_ref: None,
                evidence_ids: Vec::new(),
            },
        )
        .await
        .expect_err("a manual class has no deterministic check");
    assert!(matches!(
        mismatch,
        EffectError::ReconciliationStrategyMismatch { .. }
    ));
    let parked = f
        .ledger
        .reconcile(
            &manual.id,
            ReconciliationEvidence::Manual {
                evidence_ids: vec!["evd_manual".into()],
            },
        )
        .await
        .expect("manual reconciliation");
    assert_eq!(parked.status, EffectStatus::ReconciliationManual);
    assert_eq!(
        parked
            .reconciliation
            .as_ref()
            .expect("reconciliation")
            .strategy,
        ReconciliationStrategy::Manual
    );
    assert!(
        parked
            .reconciliation
            .as_ref()
            .expect("reconciliation")
            .attempts
            >= 1
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn retry_creates_a_new_identity_with_the_same_key() {
    let Some(f) = prepare("effects_retry").await else {
        blocked_marker();
        return;
    };
    let original = reserve_and_dispatch(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/retry"),
    )
    .await;
    let failed = f
        .ledger
        .settle_failure(&original.id, true, Some("remote-err".to_string()), vec![])
        .await
        .expect("settle failure");
    assert!(failed.outcome.as_ref().expect("outcome").is_retryable());
    let original_timeline = f.ledger.timeline(&original.id).await.expect("timeline");

    let retried = f
        .ledger
        .retry(&original.id, RetryAuthorization::RecordedDecision)
        .await
        .expect("retry");
    assert_ne!(retried.id, original.id, "a retry has a new identity");
    assert_eq!(
        retried.idempotency_key, original.idempotency_key,
        "a retry keeps the idempotency key"
    );
    assert_eq!(retried.status, EffectStatus::Reserved);
    assert_ne!(retried.dispatch_token, original.dispatch_token);

    let trail = f
        .ledger
        .find_by_key(&original.effect_class, &original.idempotency_key)
        .await
        .expect("audit trail");
    assert_eq!(trail.len(), 2, "both records remain queryable by key");
    let reloaded = f.ledger.load(&original.id).await.expect("reload original");
    assert_eq!(reloaded.status, EffectStatus::SettledFailed);
    assert_eq!(
        f.ledger
            .timeline(&original.id)
            .await
            .expect("timeline")
            .len(),
        original_timeline.len(),
        "the original record's audit trail is unchanged by the retry"
    );
    let retry_events = f.ledger.timeline(&retried.id).await.expect("timeline");
    assert_eq!(retry_events.len(), 1);
    assert_eq!(retry_events[0].event_type.to_string(), "effect.reserved");
    assert_eq!(
        retry_events[0].payload["retry_of"],
        serde_json::json!(original.id)
    );

    // A permanent failure is not retryable.
    let permanent = reserve_and_dispatch(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/permanent"),
    )
    .await;
    f.ledger
        .settle_failure(&permanent.id, false, None, vec![])
        .await
        .expect("settle permanent failure");
    let refused = f
        .ledger
        .retry(&permanent.id, RetryAuthorization::RecordedDecision)
        .await
        .expect_err("permanent failure is not retryable");
    assert!(matches!(refused, EffectError::RetryNotRetryable { .. }));

    // An EXPIRED action requires a fresh authorization, then retries as a new record.
    let expired = reserve(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/expired"),
    )
    .await;
    let expired = f.ledger.expire(&expired.id).await.expect("expire");
    assert_eq!(expired.status, EffectStatus::Expired);
    let refused = f
        .ledger
        .retry(&expired.id, RetryAuthorization::RecordedDecision)
        .await
        .expect_err("expired actions need re-authorization");
    assert!(matches!(refused, EffectError::RetryNotAuthorized { .. }));
    assert_eq!(refused.code(), "APPROVAL_REQUIRED");
    let reauthorized = f
        .ledger
        .retry(
            &expired.id,
            RetryAuthorization::Reauthorized {
                policy_decision_id: "pdc_fresh".to_string(),
            },
        )
        .await
        .expect("re-authorized retry");
    assert_ne!(reauthorized.id, expired.id);
    assert_eq!(reauthorized.idempotency_key, expired.idempotency_key);
    assert_eq!(
        reauthorized.policy_decision_id.as_deref(),
        Some("pdc_fresh")
    );

    // A denied action behaves the same way.
    let denied = reserve(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/denied"),
    )
    .await;
    f.ledger.deny(&denied.id).await.expect("deny");
    assert!(matches!(
        f.ledger
            .retry(&denied.id, RetryAuthorization::RecordedDecision)
            .await,
        Err(EffectError::RetryNotAuthorized { .. })
    ));
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn protocol_state_keeps_reconcile_effect_until_settled() {
    let Some(f) = prepare("effects_protocol").await else {
        blocked_marker();
        return;
    };
    let run = running_run(&f).await;
    let mut effect = new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/protocol");
    effect = effect
        .with_run(run.id.to_string(), None)
        .with_tool(ToolCallRef::new(
            "tc_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
            QUERY_CLASS,
        ));
    let reserved = reserve(&f.ledger, effect).await;

    let state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state exists");
    let call = state
        .pending_tool_calls
        .iter()
        .find(|call| call.effect_id == reserved.id)
        .expect("pending tool call");
    assert_eq!(call.effect_status, "RESERVED");
    assert_eq!(call.tool_name, QUERY_CLASS);
    assert_eq!(next_safe_action(&state), NextAction::Continue);

    let dispatched = dispatch(&f.ledger, &reserved).await;
    assert_eq!(
        f.ledger
            .list_for_run(&run.id.to_string(), None)
            .await
            .expect("run effects")
            .len(),
        1
    );
    assert_eq!(
        f.ledger
            .list_for_run(&run.id.to_string(), Some(EffectStatus::Reserved))
            .await
            .expect("run effects")
            .len(),
        0
    );
    let state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state exists");
    assert_eq!(pending_status(&state, &reserved.id), Some("DISPATCHED"));
    assert!(matches!(
        next_safe_action(&state),
        NextAction::ReconcileEffect { .. }
    ));

    f.ledger
        .mark_outcome_unknown(&dispatched.id, Some("timeout".to_string()))
        .await
        .expect("unknown");
    let state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state exists");
    assert_eq!(
        pending_status(&state, &reserved.id),
        Some("OUTCOME_UNKNOWN")
    );
    assert!(matches!(
        next_safe_action(&state),
        NextAction::ReconcileEffect { .. }
    ));

    f.ledger
        .reconcile(
            &reserved.id,
            ReconciliationEvidence::Determined {
                landed: true,
                remote_ref: None,
                evidence_ids: Vec::new(),
            },
        )
        .await
        .expect("reconcile");
    let state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state exists");
    assert_eq!(pending_status(&state, &reserved.id), None);
    assert_eq!(next_safe_action(&state), NextAction::Continue);

    // A manual class stays on ReconcileEffect until it is manually reconciled.
    let mut manual = new_effect(&f.ledger, WORKSPACE, MANUAL_CLASS, "host/bin/protocol");
    manual = manual
        .with_run(run.id.to_string(), None)
        .with_tool(ToolCallRef::new(
            "tc_01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
            MANUAL_CLASS,
        ));
    let manual = reserve_and_dispatch(&f.ledger, manual).await;
    f.ledger
        .mark_outcome_unknown(&manual.id, None)
        .await
        .expect("unknown");
    let state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state exists");
    assert!(matches!(
        next_safe_action(&state),
        NextAction::ReconcileEffect { .. }
    ));
    f.ledger
        .reconcile(
            &manual.id,
            ReconciliationEvidence::Manual {
                evidence_ids: vec!["evd_manual".into()],
            },
        )
        .await
        .expect("manual reconciliation");
    let state = f
        .engine
        .store()
        .load_protocol_state(&run.id)
        .await
        .expect("protocol state")
        .expect("state exists");
    assert_eq!(pending_status(&state, &manual.id), None);
    assert_eq!(next_safe_action(&state), NextAction::Continue);
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn effect_events_are_one_per_transition_in_order() {
    let Some(f) = prepare("effects_events").await else {
        blocked_marker();
        return;
    };
    let record = reserve_and_dispatch(
        &f.ledger,
        new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/events"),
    )
    .await;
    f.ledger
        .mark_outcome_unknown(&record.id, Some("timeout".to_string()))
        .await
        .expect("unknown");
    f.ledger
        .reconcile(
            &record.id,
            ReconciliationEvidence::Determined {
                landed: false,
                remote_ref: None,
                evidence_ids: Vec::new(),
            },
        )
        .await
        .expect("reconcile");

    let events = f.ledger.timeline(&record.id).await.expect("timeline");
    let types: Vec<String> = events
        .iter()
        .map(|event| event.event_type.to_string())
        .collect();
    assert_eq!(
        types,
        vec![
            "effect.proposed",
            "effect.authorized",
            "effect.reserved",
            "effect.dispatched",
            "effect.outcome_unknown",
            "effect.reconciling",
            "effect.reconciled_failed",
        ]
    );
    let versions: Vec<u64> = events.iter().map(|event| event.aggregate_version).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(
        f.ledger.load(&record.id).await.expect("record").status,
        EffectStatus::ReconciledFailed
    );
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn effect_reads_are_tenant_scoped() {
    let Some(f) = prepare("effects_isolation").await else {
        blocked_marker();
        return;
    };
    let record = dispatch(
        &f.ledger,
        &reserve(
            &f.ledger,
            new_effect(&f.ledger, WORKSPACE, QUERY_CLASS, "crm/lead/isolation"),
        )
        .await,
    )
    .await;

    assert_eq!(
        f.ledger.load(&record.id).await.expect("own load").id,
        record.id
    );
    assert_eq!(f.ledger.list_unsettled().await.expect("own list").len(), 1);

    let cross_tenant = f
        .ledger_b
        .load(&record.id)
        .await
        .expect_err("cross-tenant load must find nothing");
    assert!(matches!(cross_tenant, EffectError::NotFound { .. }));
    assert_eq!(cross_tenant.code(), "NOT_FOUND");
    assert!(f
        .ledger_b
        .timeline(&record.id)
        .await
        .expect("cross-tenant timeline")
        .is_empty());
    assert!(f
        .ledger_b
        .list_unsettled()
        .await
        .expect("cross-tenant list")
        .is_empty());
    assert!(f
        .ledger_b
        .find_by_key(&record.effect_class, &record.idempotency_key)
        .await
        .expect("cross-tenant key lookup")
        .is_empty());
    assert!(f
        .ledger_b
        .list_for_run("run_01J8Z3K6F1N8VQ2X5W9Y0EEEEE", None)
        .await
        .expect("cross-tenant run lookup")
        .is_empty());
    drop_pool(&f.pool, &f.name).await;
}

#[tokio::test]
async fn approval_required_actions_cannot_bypass_the_receipt() {
    let Some(f) = prepare("effects_approval").await else {
        blocked_marker();
        return;
    };
    let mut effect = new_effect(&f.ledger, WORKSPACE, "message.send", "mail/outbox/1");
    effect.authorization = quansio_server::effects::EffectAuthorization::ApprovalRequired;
    let refused = f
        .ledger
        .reserve(effect.clone())
        .await
        .expect_err("approval-required effects cannot reserve without a receipt");
    assert!(matches!(refused, EffectError::ApprovalRequired(_)));
    assert_eq!(refused.code(), "APPROVAL_REQUIRED");

    let proposed = f.ledger.propose(effect).await.expect("propose");
    assert_eq!(proposed.status, EffectStatus::Proposed);
    assert!(proposed.approval_receipt_id.is_none());
    let refused = f
        .ledger
        .reserve_authorized(&proposed.id)
        .await
        .expect_err("a proposed record is not reserved directly");
    assert!(matches!(refused, EffectError::IllegalTransition { .. }));
    let authorized = f
        .ledger
        .authorize(&proposed.id, "pdc_approval")
        .await
        .expect("authorize");
    assert_eq!(authorized.status, EffectStatus::Authorized);
    let refused = f
        .ledger
        .reserve_authorized(&proposed.id)
        .await
        .expect_err("no consumed receipt is bound");
    assert!(matches!(refused, EffectError::ApprovalRequired(_)));
    assert_eq!(
        f.ledger.load(&proposed.id).await.expect("reload").status,
        EffectStatus::Authorized,
        "a refused reservation changes nothing"
    );
    assert_eq!(
        effect_event_types(&f.ledger, &proposed.id).await,
        vec!["effect.proposed", "effect.authorized"]
    );
    drop_pool(&f.pool, &f.name).await;
}

fn pending_status<'a>(
    state: &'a quansio_server::runtime::protocol_state::ProtocolState,
    effect_id: &str,
) -> Option<&'a str> {
    state
        .pending_tool_calls
        .iter()
        .find(|call| call.effect_id == effect_id)
        .map(|call| call.effect_status.as_str())
}
