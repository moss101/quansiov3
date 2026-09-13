//! Egress policy against real PostgreSQL (EXEC-008).
//!
//! The three tests the task names — blocked domain, DNS rebinding, grant expiry — plus the properties
//! the two acceptance statements rest on: an unknown destination has no path to `Allow`, and a policy
//! change fences the grants issued under it.
//!
//! Every case drives `EgressStore::decide`, which is the shipped entry point: it reads the target's
//! `network_policy_id`, the policy row and the target's grants, and applies the broker's rules. When
//! the environment provides no database the suite reports `BLOCKED_EXTERNAL` and returns, so a missing
//! dev stack is never a pass.

mod common;

use std::net::IpAddr;

use quansio_machine::control::{MachineControl, NewTarget, Substrate, TargetClass};
use quansio_machine::egress::{
    AddressClass, Decision, EgressDenial, EgressError, EgressOrigin, EgressRequest, EgressStore,
    InstallOutcome, NewGrant,
};
use sqlx::{PgConnection, PgPool};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const POLICY: &str = "pol_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_POLICY: &str = "pol_01J8Z3K6F1N8VQ2X5W9Y0HHHHH";
const EFFECT: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const EFFECT_B: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0BBBBB";
const EFFECT_C: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const EFFECT_D: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0DDDDD";
const CAPABILITY: &str = "cap_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_CAPABILITY: &str = "cap_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const NOW: &str = "2026-09-13T10:00:00Z";

fn public() -> &'static [IpAddr] {
    static PUBLIC: &[IpAddr] = &[IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34))];
    PUBLIC
}

fn loopback() -> Vec<IpAddr> {
    vec!["127.0.0.1".parse().expect("literal")]
}

/// A request, built field by field so each case varies only what it is about.
struct Spec<'a> {
    tenant_id: &'a str,
    target_id: &'a str,
    target_generation: i64,
    capability_id: Option<&'a str>,
    origin: EgressOrigin,
    url: &'a str,
    resolved_addresses: &'a [IpAddr],
    headers: &'a [(&'a str, &'a str)],
    credential_ref: Option<&'a str>,
}

impl<'a> Spec<'a> {
    fn new(url: &'a str, resolved_addresses: &'a [IpAddr]) -> Self {
        Self {
            tenant_id: TENANT,
            target_id: TARGET,
            target_generation: 1,
            capability_id: None,
            origin: EgressOrigin::Tool,
            url,
            resolved_addresses,
            headers: &[],
            credential_ref: None,
        }
    }

    fn build(&self) -> EgressRequest<'a> {
        EgressRequest {
            tenant_id: self.tenant_id,
            target_id: self.target_id,
            target_generation: self.target_generation,
            capability_id: self.capability_id,
            origin: self.origin,
            url: self.url,
            resolved_addresses: self.resolved_addresses,
            at: NOW,
            headers: self.headers,
            credential_ref: self.credential_ref,
        }
    }
}

/// A grant request for `host:port`, issued for the seeded target at generation 1.
fn grant(effect_id: &'static str, host: &'static str, port: u16) -> NewGrant<'static> {
    NewGrant {
        effect_id,
        tenant_id: TENANT,
        target_id: TARGET,
        target_generation: 1,
        capability_id: None,
        host,
        port,
        issued_at: "2026-09-13T09:55:00Z",
        expires_at: "2026-09-13T10:30:00Z",
    }
}

async fn decide(conn: &mut PgConnection, spec: &Spec<'_>) -> (Decision, String) {
    let (decision, record) = EgressStore::decide(conn, &spec.build())
        .await
        .expect("the broker answered");
    (decision, record.to_string())
}

/// Seed the `network.egress.new_destination` effect a grant is the settled outcome of.
///
/// `egress_grants.effect_id` references `effect_records`, and that is deliberate: a grant *is* what the
/// ledger's effect settled, which is what makes re-issuing it idempotent. The ledger is seeded here as
/// fixture rather than through its owner, because `crates/machine` must not depend on the server crate;
/// what the test drives on the real path is the shipped `EgressStore`.
async fn seed_effect(conn: &mut PgConnection, effect_id: &str) {
    sqlx::query(
        "INSERT INTO effect_records (id, tenant_id, workspace_id, effect_class, tier, resource, \
                                     params_digest, idempotency_key, capability_projection_id, \
                                     status, generation, settled_at) \
         VALUES ($1, $2, $3, 'network.egress.new_destination', 2, $4, $5, $6, $7, \
                 'SETTLED_SUCCESS', 1, now()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(effect_id)
    .bind(TENANT)
    .bind(WORKSPACE)
    .bind(serde_json::json!({ "kind": "domain", "value": "api.example.com" }))
    .bind("0".repeat(64))
    .bind(effect_id)
    .bind(CAPABILITY)
    .execute(&mut *conn)
    .await
    .expect("seed the authorizing effect");
}

/// Settle an effect and issue its grant, which is what the runtime does for a granted destination.
async fn issue(
    conn: &mut PgConnection,
    grant: &NewGrant<'_>,
) -> Result<InstallOutcome, EgressError> {
    seed_effect(conn, grant.effect_id).await;
    EgressStore::issue(conn, grant).await
}

/// Seed two tenants, two execution targets, and the network policy the first target is bound to.
async fn seed(pool: &PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    common::seed_tenant(pool, TENANT, USER, WORKSPACE).await;
    common::seed_tenant(pool, OTHER_TENANT, OTHER_USER, OTHER_WORKSPACE).await;
    let mut pooled = pool.acquire().await.expect("acquire");
    let conn: &mut PgConnection = &mut pooled;

    for id in [TARGET, OTHER_TARGET] {
        MachineControl::register(
            conn,
            TENANT,
            &NewTarget {
                id: id.to_string(),
                workspace_id: WORKSPACE.to_string(),
                class: TargetClass::IsolatedTaskRuntime,
                substrate: Substrate::CloudMicrovm,
                image_digest: None,
                desired_state: None,
            },
        )
        .await
        .expect("register target");
    }

    // The network policy is a canonical `policies` row (DOMAIN.md §7.3): the broker reads its rule for
    // `network.egress.new_destination` and takes `version` as the revision grants are fenced on.
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules, version) \
         VALUES ($1, $2, NULL, 'tenant', $3, 1)",
    )
    .bind(POLICY)
    .bind(TENANT)
    .bind(network_rules("ask", &["public"]))
    .execute(&mut *conn)
    .await
    .expect("insert policy");
    MachineControl::set_network_policy(conn, TENANT, TARGET, Some(POLICY))
        .await
        .expect("bind policy");
    pooled
}

fn network_rules(decision: &str, classes: &[&str]) -> serde_json::Value {
    serde_json::json!([{
        "effect_class": "network.egress.new_destination",
        "decision": decision,
        "conditions": { "allowed_address_classes": classes }
    }])
}

// ------------------------------------------------------------------ deny by default

#[tokio::test]
async fn an_unknown_destination_is_denied_until_an_effect_grants_it() {
    let name = common::scratch_name("egress_default");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // Nothing is granted, so nothing is reachable -- and the log says which rule refused it.
    let (decision, log) = decide(conn, &Spec::new("https://api.example.com/v1", public())).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::NoGrant {
            host: "api.example.com".to_string(),
            port: 443,
        })
    );
    assert!(log.contains("decision=no_grant"), "{log}");

    // The tier-2 effect settles and its grant is issued, pinning the target's current policy revision.
    let outcome = issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("issue");
    assert!(outcome.installed());
    assert_eq!(outcome.grant().policy_id, POLICY);
    assert_eq!(
        outcome.grant().policy_version,
        1,
        "the pin is read from the policy, not supplied by the caller"
    );

    let (decision, log) = decide(conn, &Spec::new("https://api.example.com/v1", public())).await;
    assert_eq!(
        decision,
        Decision::Allow {
            effect_id: EFFECT.to_string()
        }
    );
    assert!(log.contains("decision=allow"), "{log}");
    assert!(log.contains("destination=api.example.com:443"), "{log}");

    // It is granted for that destination only: another host, and another port, are still denied.
    for (url, host, port) in [
        ("https://other.example.com/", "other.example.com", 443),
        ("https://api.example.com:8443/", "api.example.com", 8443),
    ] {
        let (decision, _) = decide(conn, &Spec::new(url, public())).await;
        assert_eq!(
            decision,
            Decision::Deny(EgressDenial::NoGrant {
                host: host.to_string(),
                port,
            }),
            "{url}"
        );
    }

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ blocked domain

#[tokio::test]
async fn a_blocked_domain_is_refused_even_with_a_grant() {
    let name = common::scratch_name("egress_blocked");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // Grant the internal names deliberately: the point is that a grant cannot make them reachable.
    let cases: [(&str, &str, u16, &str); 3] = [
        ("localhost", "http://localhost:9000/", 9000, EFFECT),
        ("vault.internal", "https://vault.internal/", 443, EFFECT_B),
        ("printer.local", "https://printer.local/", 443, EFFECT_C),
    ];
    for (host, url, port, effect_id) in cases {
        issue(conn, &grant(effect_id, host, port))
            .await
            .expect("issue for a blocked name");
        let (decision, log) = decide(conn, &Spec::new(url, public())).await;
        assert!(
            matches!(decision, Decision::Deny(EgressDenial::BlockedHost { .. })),
            "{url} must be blocked, got {decision:?}"
        );
        assert!(log.contains("decision=blocked_host"), "{log}");
    }

    // A literal metadata address is refused by its class, and the name it is reached by is refused by
    // name: neither half of the rule depends on the other.
    let metadata: Vec<IpAddr> = vec!["169.254.169.254".parse().expect("literal")];
    let (decision, _) = decide(conn, &Spec::new("http://metadata.example.com/", &metadata)).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::AddressNotAllowed {
            host: "metadata.example.com".to_string(),
            address: metadata[0],
            class: AddressClass::LinkLocal,
        })
    );

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ DNS rebinding

#[tokio::test]
async fn a_granted_name_that_resolves_inside_is_refused() {
    let name = common::scratch_name("egress_rebind");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("issue");

    // The name is granted and resolves to a public address: reached.
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert!(decision.is_allowed(), "got {decision:?}");

    // The same granted name now resolves to the machine itself. The grant is for the *name*, and it
    // cannot make the address acceptable, so nothing is reached.
    let inside = loopback();
    let (decision, log) = decide(conn, &Spec::new("https://api.example.com/", &inside)).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::AddressNotAllowed {
            host: "api.example.com".to_string(),
            address: inside[0],
            class: AddressClass::Loopback,
        })
    );
    assert!(log.contains("decision=address_not_allowed"), "{log}");

    // Every address the name resolved to is checked, so one internal answer refuses a mixed set.
    let mixed: Vec<IpAddr> = vec![public()[0], "169.254.169.254".parse().expect("literal")];
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", &mixed)).await;
    assert!(matches!(
        decision,
        Decision::Deny(EgressDenial::AddressNotAllowed { .. })
    ));

    // A name that did not resolve is refused rather than assumed fine: not knowing the addresses is not
    // the same as knowing they are allowed.
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", &[])).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::Unresolved {
            host: "api.example.com".to_string()
        })
    );

    // A policy that deliberately allows the class does reach it, which is what shows the refusal above
    // is the address rule rather than a blanket ban.
    sqlx::query("UPDATE policies SET rules = $2, version = 2 WHERE id = $1 AND tenant_id = $3")
        .bind(POLICY)
        .bind(network_rules("ask", &["public", "loopback"]))
        .bind(TENANT)
        .execute(&mut *conn)
        .await
        .expect("widen the policy");
    // The revision moved, so the grant issued under it is fenced: re-issue under the new policy.
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert!(matches!(
        decision,
        Decision::Deny(EgressDenial::GrantFenced { .. })
    ));
    issue(conn, &grant(EFFECT_B, "api.example.com", 443))
        .await
        .expect("re-issue");
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", &inside)).await;
    assert!(decision.is_allowed(), "got {decision:?}");

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ grant expiry

#[tokio::test]
async fn a_grant_stops_being_usable_when_it_expires() {
    let name = common::scratch_name("egress_expiry");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // A grant that expired before the request.
    issue(
        conn,
        &NewGrant {
            issued_at: "2026-09-13T09:00:00Z",
            expires_at: "2026-09-13T09:30:00Z",
            ..grant(EFFECT, "api.example.com", 443)
        },
    )
    .await
    .expect("issue");
    let (decision, log) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::GrantExpired {
            host: "api.example.com".to_string(),
            port: 443,
            expires_at: "2026-09-13T09:30:00Z".to_string(),
        })
    );
    assert!(log.contains("decision=grant_expired"), "{log}");

    // Expiry is exclusive: a grant is not usable at the instant it expires.
    issue(
        conn,
        &NewGrant {
            issued_at: "2026-09-13T09:00:00Z",
            expires_at: NOW,
            ..grant(EFFECT_B, "api.example.com", 443)
        },
    )
    .await
    .expect("issue");
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert!(matches!(
        decision,
        Decision::Deny(EgressDenial::GrantExpired { .. })
    ));

    // A grant whose window does not move forward is refused with a named rule rather than as a
    // constraint violation, and nothing is written.
    assert!(matches!(
        issue(
            conn,
            &NewGrant {
                issued_at: "2026-09-13T10:00:00Z",
                expires_at: "2026-09-13T09:00:00Z",
                ..grant(EFFECT_C, "api.example.com", 443)
            }
        )
        .await,
        Err(EgressError::WindowInvalid { .. })
    ));
    assert_eq!(
        EgressStore::grants_for(conn, TENANT, TARGET)
            .await
            .expect("list")
            .len(),
        2,
        "the refused grant was not written"
    );

    // Expired grants are walked past: a live grant for the same destination is what entitles a caller.
    issue(conn, &grant(EFFECT_C, "api.example.com", 443))
        .await
        .expect("issue a live grant");
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert_eq!(
        decision,
        Decision::Allow {
            effect_id: EFFECT_C.to_string()
        }
    );

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ policy change

#[tokio::test]
async fn a_policy_change_fences_the_grants_issued_under_it() {
    let name = common::scratch_name("egress_fence");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("issue");
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert!(decision.is_allowed(), "got {decision:?}");

    // The policy owner changes the policy. Nothing scans or rewrites the grant: `policies.version`
    // moved, and the grant records the revision it was issued under.
    sqlx::query("UPDATE policies SET version = 2 WHERE id = $1 AND tenant_id = $2")
        .bind(POLICY)
        .bind(TENANT)
        .execute(&mut *conn)
        .await
        .expect("bump the policy revision");
    let (decision, log) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::GrantFenced {
            host: "api.example.com".to_string(),
            port: 443,
            issued: 1,
            current: 2,
        })
    );
    assert!(log.contains("decision=grant_fenced"), "{log}");

    // Re-issuing under the new revision works, and the grant pins the revision it was written at.
    let outcome = issue(conn, &grant(EFFECT_B, "api.example.com", 443))
        .await
        .expect("re-issue");
    assert_eq!(outcome.grant().policy_version, 2);
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert_eq!(
        decision,
        Decision::Allow {
            effect_id: EFFECT_B.to_string()
        }
    );

    // Rebinding the target to a different policy is the other half: the grant names a policy, so a
    // grant for another one is not this target's.
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules, version) \
         VALUES ($1, $2, NULL, 'tenant', $3, 1)",
    )
    .bind(OTHER_POLICY)
    .bind(TENANT)
    .bind(serde_json::json!([{
        "effect_class": "network.egress.new_destination",
        "decision": "ask",
        "conditions": { "allowed_address_classes": ["public"] }
    }]))
    .execute(&mut *conn)
    .await
    .expect("insert the second policy");
    MachineControl::set_network_policy(conn, TENANT, TARGET, Some(OTHER_POLICY))
        .await
        .expect("rebind");
    let (decision, log) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::GrantRebound {
            host: "api.example.com".to_string(),
            port: 443,
            from: POLICY.to_string(),
            to: OTHER_POLICY.to_string(),
        })
    );
    assert!(log.contains("decision=grant_rebound"), "{log}");

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ revocation

#[tokio::test]
async fn revocation_is_recorded_rather_than_deleted() {
    let name = common::scratch_name("egress_revoke");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("issue");

    EgressStore::revoke(conn, TENANT, EFFECT, "2026-09-13T09:58:00Z")
        .await
        .expect("revoke");
    let (decision, log) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::GrantRevoked {
            host: "api.example.com".to_string(),
            port: 443,
            revoked_at: "2026-09-13T09:58:00Z".to_string(),
        })
    );
    assert!(log.contains("decision=grant_revoked"), "{log}");

    // The row is still there: revocation is a state, not a deletion, so the audit trail survives.
    let stored = EgressStore::grants_for(conn, TENANT, TARGET)
        .await
        .expect("list");
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].revoked_at.as_deref(),
        Some("2026-09-13T09:58:00Z")
    );

    // Revoking a target's grants is one statement and reports what it changed.
    issue(conn, &grant(EFFECT_B, "other.example.com", 443))
        .await
        .expect("issue");
    let revoked = EgressStore::revoke_for_target(conn, TENANT, TARGET, "2026-09-13T09:59:00Z")
        .await
        .expect("revoke all");
    assert_eq!(revoked, 1, "only the grant that was still live");

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ idempotency and conflicts

#[tokio::test]
async fn a_grant_is_keyed_by_the_effect_that_authorized_it() {
    let name = common::scratch_name("egress_idem");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    let first = issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("issue");
    assert!(first.installed());
    let again = issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("re-issue");
    assert!(
        matches!(again, InstallOutcome::AlreadyInstalled(_)),
        "a settled effect must not acquire a second grant"
    );
    assert_eq!(again.grant(), first.grant());
    assert_eq!(
        EgressStore::grants_for(conn, TENANT, TARGET)
            .await
            .expect("list")
            .len(),
        1
    );

    // The same effect cannot be reused for another target: that would be one effect meaning two
    // different things.
    assert!(matches!(
        issue(
            conn,
            &NewGrant {
                target_id: OTHER_TARGET,
                ..grant(EFFECT, "api.example.com", 443)
            }
        )
        .await,
        Err(EgressError::GrantConflict { .. })
    ));

    // A host that is not a canonical host is refused with a named rule, before any write.
    assert!(matches!(
        issue(conn, &grant(EFFECT_D, "API.example.com", 443)).await,
        Err(EgressError::HostInvalid(_))
    ));

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------- generation, capability and tenant scope

#[tokio::test]
async fn a_grant_is_fenced_by_the_target_generation_and_narrowed_by_capability() {
    let name = common::scratch_name("egress_scope");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("issue");

    // The target is replaced, which moves its generation. The grant was for the old machine.
    sqlx::query("UPDATE execution_targets SET generation = 2 WHERE id = $1 AND tenant_id = $2")
        .bind(TARGET)
        .bind(TENANT)
        .execute(&mut *conn)
        .await
        .expect("bump the generation");
    let later = Spec {
        target_generation: 2,
        ..Spec::new("https://api.example.com/", public())
    };
    let (decision, log) = decide(conn, &later).await;
    assert_eq!(
        decision,
        Decision::Deny(EgressDenial::GrantGenerationStale {
            host: "api.example.com".to_string(),
            port: 443,
            issued: 1,
            current: 2,
        })
    );
    assert!(log.contains("decision=grant_generation_stale"), "{log}");

    // A grant narrowed to one capability is not another capability's to use.
    issue(
        conn,
        &NewGrant {
            target_generation: 2,
            capability_id: Some(CAPABILITY),
            ..grant(EFFECT_B, "api.example.com", 443)
        },
    )
    .await
    .expect("issue");
    let asking = Spec {
        capability_id: Some(OTHER_CAPABILITY),
        ..later
    };
    let (decision, _) = decide(conn, &asking).await;
    assert!(matches!(
        decision,
        Decision::Deny(EgressDenial::GrantNotForCapability { .. })
    ));
    let matching = Spec {
        capability_id: Some(CAPABILITY),
        ..asking
    };
    let (decision, _) = decide(conn, &matching).await;
    assert!(decision.is_allowed(), "got {decision:?}");

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn another_tenant_cannot_reach_or_change_this_tenants_grants() {
    let name = common::scratch_name("egress_tenant");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    issue(conn, &grant(EFFECT, "api.example.com", 443))
        .await
        .expect("issue");

    // The other tenant has no such target, so the broker cannot even read a policy for it.
    let foreign = Spec {
        tenant_id: OTHER_TENANT,
        ..Spec::new("https://api.example.com/", public())
    };
    assert!(matches!(
        EgressStore::decide(conn, &foreign.build()).await,
        Err(EgressError::TargetNotFound(_))
    ));

    // Its grant list is empty, and revoking through it changes nothing.
    assert!(EgressStore::grants_for(conn, OTHER_TENANT, TARGET)
        .await
        .expect("list")
        .is_empty());
    EgressStore::revoke(conn, OTHER_TENANT, EFFECT, "2026-09-13T09:59:00Z")
        .await
        .expect("a no-op, not an error");
    let stored = EgressStore::grants_for(conn, TENANT, TARGET)
        .await
        .expect("list");
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].revoked_at.is_none(),
        "another tenant revoked a grant that is not its own"
    );

    // And the tenant that owns it still reaches the destination.
    let (decision, _) = decide(conn, &Spec::new("https://api.example.com/", public())).await;
    assert!(decision.is_allowed(), "got {decision:?}");

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_target_with_no_network_policy_reaches_nothing() {
    let name = common::scratch_name("egress_nopolicy");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // The second target was registered without a policy. Deny by default, and for a named reason: it
    // has nothing to be governed by, which is not the same as a policy that says no.
    let bare = Spec {
        target_id: OTHER_TARGET,
        ..Spec::new("https://api.example.com/", public())
    };
    assert!(matches!(
        EgressStore::decide(conn, &bare.build()).await,
        Err(EgressError::PolicyMissing(_))
    ));
    assert!(matches!(
        issue(
            conn,
            &NewGrant {
                target_id: OTHER_TARGET,
                ..grant(EFFECT, "api.example.com", 443)
            }
        )
        .await,
        Err(EgressError::PolicyMissing(_))
    ));

    // Binding a policy makes it governable, and the pin is read from it.
    MachineControl::set_network_policy(conn, TENANT, OTHER_TARGET, Some(POLICY))
        .await
        .expect("bind");
    let outcome = issue(
        conn,
        &NewGrant {
            target_id: OTHER_TARGET,
            ..grant(EFFECT, "api.example.com", 443)
        },
    )
    .await
    .expect("issue");
    assert_eq!(outcome.grant().policy_version, 1);

    // A request for a target the tenant does not have is a typed refusal rather than a panic.
    assert!(matches!(
        EgressStore::policy_for(conn, TENANT, "tgt_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ").await,
        Err(EgressError::TargetNotFound(_))
    ));

    // Clearing the binding takes the target back to reaching nothing.
    MachineControl::set_network_policy(conn, TENANT, TARGET, None)
        .await
        .expect("unbind");
    assert!(matches!(
        EgressStore::decide(
            conn,
            &Spec::new("https://api.example.com/", public()).build()
        )
        .await,
        Err(EgressError::PolicyMissing(_))
    ));

    common::drop_pool(&pool, &name).await;
}
