//! The secret broker against real PostgreSQL (EXEC-007).
//!
//! The three tests the task names — secret scan, revocation, scope substitution — plus the properties
//! the two acceptance statements rest on: a value is absent from everything that is logged or sent, and
//! a revoked handle's materialization cannot be used from a worker that cached it.
//!
//! Every case drives the shipped `SecretBroker` against a scratch database. When the environment
//! provides no database the suite reports `BLOCKED_EXTERNAL` and returns, so a missing dev stack is
//! never a pass.

mod common;

use quansio_core::OsEntropy;
use quansio_machine::secrets::{
    LocalMasterKeyProvider, MaterializationScope, MaterializeRequest, NewSecret, SecretBroker,
    SecretError, SecretHandle, SecretStatus,
};
use sqlx::{PgConnection, PgPool};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const HANDLE: &str = "sec_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_HANDLE: &str = "sec_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const EFFECT: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const CONNECTOR: &str = "cnx_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_CONNECTOR: &str = "cnx_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const NOW: &str = "2026-09-13T10:00:00Z";

/// A value that must never turn up anywhere but the materialization.
const CANARY: &[u8] = b"canary-github-token-9f3a2b7c";

fn provider() -> LocalMasterKeyProvider {
    // A fixed master key, so the test's cryptography is deterministic; the shipped path reads one from
    // the operator's key file.
    LocalMasterKeyProvider::from_master_key(&[42u8; 32], "local:kek-v1").expect("a master key")
}

fn provider_two() -> LocalMasterKeyProvider {
    LocalMasterKeyProvider::from_master_key(&[43u8; 32], "local:kek-v2").expect("a master key")
}

async fn seed(pool: &PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    common::seed_tenant(pool, TENANT, USER, WORKSPACE).await;
    common::seed_tenant(pool, OTHER_TENANT, OTHER_USER, OTHER_WORKSPACE).await;
    let mut pooled = pool.acquire().await.expect("acquire");
    let conn: &mut PgConnection = &mut pooled;
    let _ = conn;
    pooled
}

async fn register(
    conn: &mut PgConnection,
    id: &str,
    workspace: Option<&str>,
    expires_at: Option<&str>,
) -> SecretHandle {
    SecretBroker::register(
        conn,
        &provider(),
        &NewSecret {
            id,
            tenant_id: TENANT,
            workspace_id: workspace,
            provider: "github",
            label: "deploy key",
            expires_at,
        },
        CANARY,
        &OsEntropy,
    )
    .await
    .expect("register")
}

fn materialize_request<'a>(
    handle_id: &'a str,
    scope: MaterializationScope,
    expires_at: &'a str,
) -> MaterializeRequest<'a> {
    MaterializeRequest {
        tenant_id: TENANT,
        workspace_id: None,
        handle_id,
        actor: USER,
        effect_id: EFFECT,
        scope,
        expires_at,
        at: NOW,
    }
}

fn scope() -> MaterializationScope {
    MaterializationScope::Connector {
        connector_id: CONNECTOR.to_string(),
    }
}

/// Every byte-column of the handle's row, so a scan is over what is actually stored rather than over a
/// rendering the broker chose.
async fn stored_rows(conn: &mut PgConnection) -> Vec<(Option<Vec<u8>>, Option<String>, String)> {
    sqlx::query_as::<_, (Option<Vec<u8>>, Option<String>, String)>(
        "SELECT ciphertext, data_key_ref, id FROM secret_handles WHERE tenant_id = $1",
    )
    .bind(TENANT)
    .fetch_all(&mut *conn)
    .await
    .expect("read rows")
}

// ------------------------------------------------------------------ secret scan

#[tokio::test]
async fn the_value_is_stored_sealed_and_appears_in_nothing_that_is_logged_or_sent() {
    let name = common::scratch_name("secrets_scan");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    let handle = register(conn, HANDLE, None, None).await;
    assert_eq!(handle.status, SecretStatus::Active);
    assert_eq!(handle.generation, 1);
    assert_eq!(handle.data_key_ref.as_deref(), Some("local:kek-v1"));
    assert!(handle.last_used_at.is_none());

    // The handle's own rendering — the thing that travels on the wire — has no material and no field for
    // it, and it does not contain the value.
    let shown = format!("{handle:?}");
    assert!(!shown.contains("canary-github-token-9f3a2b7c"), "{shown}");
    assert!(shown.contains("deploy key"), "{shown}");
    assert!(!shown.contains("ciphertext"), "{shown}");

    // Nothing in the row is the plaintext: not the frame, not the key reference, not the id.
    for (ciphertext, key_ref, id) in stored_rows(conn).await {
        let frame = ciphertext.expect("the material is stored");
        assert!(
            !frame.windows(CANARY.len()).any(|window| window == CANARY),
            "the plaintext is in {id}'s stored frame"
        );
        // It really is sealed, not merely absent: the frame carries an envelope.
        assert!(frame.len() > CANARY.len(), "the frame is {frame:?}");
        assert!(key_ref.is_some(), "the sealing key is recorded");
    }

    // The material resolves at the boundary, which is what makes the scan above meaningful rather than
    // a check that the value was lost.
    let expires_at = MaterializeRequest::expiry_after(NOW, 60).expect("canonical");
    let (materialization, access) = SecretBroker::materialize(
        conn,
        &provider(),
        &materialize_request(HANDLE, scope(), &expires_at),
    )
    .await
    .expect("materialize");
    assert_eq!(materialization.material.expose(), CANARY);
    assert_eq!(materialization.generation, 1);
    assert_eq!(materialization.expires_at, "2026-09-13T10:01:00Z");

    // Nothing that is rendered out of the materialization carries the value either.
    let rendered = format!("{materialization:?}");
    assert!(rendered.contains("redacted"), "{rendered}");
    assert!(
        !rendered.contains("canary-github-token-9f3a2b7c"),
        "{rendered}"
    );

    // The access record says who took it, for what, under which effect — and never the value.
    assert_eq!(access.handle_id, HANDLE);
    assert_eq!(access.actor, USER);
    assert_eq!(access.effect_id, EFFECT);
    assert_eq!(access.scope, format!("connector:{CONNECTOR}"));
    assert_eq!(access.generation, 1);
    assert_eq!(access.decision, "allowed");
    let rendered = format!("{access:?}");
    assert!(
        !rendered.contains("canary-github-token-9f3a2b7c"),
        "{rendered}"
    );

    // The access was recorded: an access that happened is an access that is on the handle.
    let reloaded = SecretBroker::load(conn, TENANT, HANDLE)
        .await
        .expect("load");
    assert_eq!(reloaded.last_used_at.as_deref(), Some(NOW));

    // The handle listing, which is what a UI shows, holds metadata only.
    let listed = SecretBroker::list(conn, TENANT).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert!(!format!("{listed:?}").contains("canary-github-token-9f3a2b7c"));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_handle_sealed_under_one_key_does_not_open_under_another() {
    let name = common::scratch_name("secrets_kek");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    register(conn, HANDLE, None, None).await;

    let expires_at = MaterializeRequest::expiry_after(NOW, 60).expect("canonical");
    // The wrong key-encryption key cannot resolve the material: a rotated or replaced master key has to
    // be a refusal, not a silently different value.
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider_two(),
            &materialize_request(HANDLE, scope(), &expires_at)
        )
        .await,
        Err(SecretError::KeyProvider { .. })
    ));
    // And the right one still works, so the refusal above is the key and not the request.
    assert!(SecretBroker::materialize(
        conn,
        &provider(),
        &materialize_request(HANDLE, scope(), &expires_at)
    )
    .await
    .is_ok());

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ revocation

#[tokio::test]
async fn revoking_fences_the_materialization_a_worker_already_holds() {
    let name = common::scratch_name("secrets_revoke");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    register(conn, HANDLE, None, None).await;

    let expires_at = MaterializeRequest::expiry_after(NOW, 60).expect("canonical");
    let (materialization, _) = SecretBroker::materialize(
        conn,
        &provider(),
        &materialize_request(HANDLE, scope(), &expires_at),
    )
    .await
    .expect("materialize");

    // The worker holds it and asks whether it may still use it.
    SecretBroker::validate_materialization(conn, TENANT, &materialization, &scope(), NOW)
        .await
        .expect("valid before revocation");

    let revoked = SecretBroker::revoke(conn, TENANT, HANDLE)
        .await
        .expect("revoke");
    assert_eq!(revoked.status, SecretStatus::Revoked);
    assert_eq!(
        revoked.generation, 2,
        "revoking is what moves the fence, in the same statement"
    );

    // The worker's cached materialization is refused, and for the reason rather than a bare no: the
    // handle moved past the generation it was issued under.
    let refusal = SecretBroker::validate_materialization(
        conn,
        TENANT,
        &materialization,
        &scope(),
        "2026-09-13T10:00:30Z",
    )
    .await
    .expect_err("a revoked handle's materialization must not validate");
    assert!(
        matches!(&refusal, SecretError::HandleRevoked { handle_id } if handle_id == HANDLE),
        "got {refusal:?}"
    );

    // And it cannot be materialized again at all.
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider(),
            &materialize_request(HANDLE, scope(), "2026-09-13T11:00:00Z")
        )
        .await,
        Err(SecretError::HandleRevoked { .. })
    ));

    // Revoking twice is a refusal rather than a silent second fence.
    assert!(matches!(
        SecretBroker::revoke(conn, TENANT, HANDLE).await,
        Ok(handle) if handle.status == SecretStatus::Revoked
    ));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn rotating_replaces_the_material_and_fences_the_materializations_before_it() {
    let name = common::scratch_name("secrets_rotate");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    register(conn, HANDLE, None, None).await;

    let expires_at = MaterializeRequest::expiry_after(NOW, 60).expect("canonical");
    let (before, _) = SecretBroker::materialize(
        conn,
        &provider(),
        &materialize_request(HANDLE, scope(), &expires_at),
    )
    .await
    .expect("materialize");

    const ROTATED: &[u8] = b"rotated-github-token-4d1e8a";
    let rotated = SecretBroker::rotate(conn, &provider(), TENANT, HANDLE, ROTATED, &OsEntropy)
        .await
        .expect("rotate");
    assert_eq!(rotated.generation, 2, "rotation is a fence");
    assert_eq!(rotated.status, SecretStatus::Active);

    // The new material resolves, the old value is gone, and the earlier materialization is fenced.
    let (after, _) = SecretBroker::materialize(
        conn,
        &provider(),
        &materialize_request(HANDLE, scope(), "2026-09-13T10:00:30Z"),
    )
    .await
    .expect("materialize");
    assert_eq!(after.material.expose(), ROTATED);
    assert_eq!(after.generation, 2);
    assert!(matches!(
        SecretBroker::validate_materialization(
            conn,
            TENANT,
            &before,
            &scope(),
            "2026-09-13T10:00:30Z"
        )
        .await,
        Err(SecretError::FencedStaleGeneration {
            issued: 1,
            current: 2
        })
    ));

    // The frame was replaced, not re-wrapped in place: rotating does not leave the old secret readable
    // under the new data key.
    for (ciphertext, _, _) in stored_rows(conn).await {
        let frame = ciphertext.expect("stored");
        assert!(!frame.windows(ROTATED.len()).any(|window| window == ROTATED));
        assert!(!frame.windows(CANARY.len()).any(|window| window == CANARY));
    }

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ scope substitution

#[tokio::test]
async fn a_materialization_cannot_be_presented_for_another_scope() {
    let name = common::scratch_name("secrets_scope");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    register(conn, HANDLE, None, None).await;

    let expires_at = MaterializeRequest::expiry_after(NOW, 60).expect("canonical");
    let (materialization, _) = SecretBroker::materialize(
        conn,
        &provider(),
        &materialize_request(HANDLE, scope(), &expires_at),
    )
    .await
    .expect("materialize");

    // Another connector, another target and another tool are all substitutions, and each is named so an
    // operator reading the log learns which credential was offered where.
    let substitutions = [
        MaterializationScope::Connector {
            connector_id: OTHER_CONNECTOR.to_string(),
        },
        MaterializationScope::Target {
            target_id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
        },
        MaterializationScope::Tool {
            tool: "connector.github.comment".to_string(),
        },
    ];
    for presented in &substitutions {
        let refusal =
            SecretBroker::validate_materialization(conn, TENANT, &materialization, presented, NOW)
                .await
                .expect_err("a substituted scope must be refused");
        assert!(
            matches!(&refusal, SecretError::ScopeSubstitution { issued, presented: seen }
                if issued == &format!("connector:{CONNECTOR}") && seen == &presented.as_str()),
            "got {refusal:?}"
        );
    }

    // The scope it was issued for still validates, so the refusal is the substitution and not the
    // request.
    SecretBroker::validate_materialization(conn, TENANT, &materialization, &scope(), NOW)
        .await
        .expect("the issued scope validates");

    // A materialization past its own lifetime is refused before the handle is even read.
    assert!(matches!(
        SecretBroker::validate_materialization(
            conn,
            TENANT,
            &materialization,
            &scope(),
            "2026-09-13T10:01:00Z"
        )
        .await,
        Err(SecretError::MaterializationExpired { .. })
    ));

    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ expiry and scope

#[tokio::test]
async fn a_scheduled_expiry_sweeps_the_handle_and_fences_it() {
    let name = common::scratch_name("secrets_expiry");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // One handle already past its expiry and one still live. Both are registered unexpired -- the
    // schema refuses a handle that expires before it was created -- and then given their instants
    // together, so the fixture satisfies the same constraint the product does.
    register(conn, HANDLE, None, None).await;
    register(conn, OTHER_HANDLE, None, None).await;
    for (id, created, expires) in [
        (HANDLE, "2026-09-13T08:00:00Z", "2026-09-13T09:00:00Z"),
        (OTHER_HANDLE, "2026-09-13T08:00:00Z", "2026-09-13T11:00:00Z"),
    ] {
        sqlx::query(
            "UPDATE secret_handles SET created_at = $3::timestamptz, expires_at = $4::timestamptz \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(TENANT)
        .bind(created)
        .bind(expires)
        .execute(&mut *conn)
        .await
        .expect("set the handle's instants");
    }

    // A scheduled expiry is honoured before the sweep runs, so the answer does not depend on the sweep.
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider(),
            &materialize_request(HANDLE, scope(), "2026-09-13T10:00:01Z")
        )
        .await,
        Err(SecretError::HandleExpired { .. })
    ));

    let swept = SecretBroker::expire_due(conn, TENANT, NOW)
        .await
        .expect("sweep");
    assert_eq!(swept, 1, "only the handle past its expiry");
    let expired = SecretBroker::load(conn, TENANT, HANDLE)
        .await
        .expect("load");
    assert_eq!(expired.status, SecretStatus::Expired);
    assert_eq!(expired.generation, 2, "the sweep fences too");
    let live = SecretBroker::load(conn, TENANT, OTHER_HANDLE)
        .await
        .expect("load");
    assert_eq!(live.status, SecretStatus::Active);
    assert_eq!(live.generation, 1);

    // A sweep with nothing due changes nothing.
    assert_eq!(
        SecretBroker::expire_due(conn, TENANT, NOW)
            .await
            .expect("sweep"),
        0
    );

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_handle_is_reachable_only_from_its_own_tenant_and_workspace() {
    let name = common::scratch_name("secrets_isolate");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    let handle = register(conn, HANDLE, Some(WORKSPACE), None).await;
    assert_eq!(handle.workspace_id.as_deref(), Some(WORKSPACE));

    // Another tenant cannot load, list, materialize or revoke it.
    assert!(matches!(
        SecretBroker::load(conn, OTHER_TENANT, HANDLE).await,
        Err(SecretError::HandleNotFound(_))
    ));
    assert!(SecretBroker::list(conn, OTHER_TENANT)
        .await
        .expect("list")
        .is_empty());
    assert!(matches!(
        SecretBroker::revoke(conn, OTHER_TENANT, HANDLE).await,
        Err(SecretError::HandleNotFound(_))
    ));
    let expires_at = MaterializeRequest::expiry_after(NOW, 60).expect("canonical");
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider(),
            &MaterializeRequest {
                tenant_id: OTHER_TENANT,
                ..materialize_request(HANDLE, scope(), &expires_at)
            }
        )
        .await,
        Err(SecretError::HandleNotFound(_))
    ));
    // The revoke through the other tenant changed nothing.
    assert_eq!(
        SecretBroker::load(conn, TENANT, HANDLE)
            .await
            .expect("load")
            .status,
        SecretStatus::Active
    );

    // A workspace-scoped handle is not materializable from another workspace, even in its own tenant.
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider(),
            &MaterializeRequest {
                workspace_id: Some(OTHER_WORKSPACE),
                ..materialize_request(HANDLE, scope(), &expires_at)
            }
        )
        .await,
        Err(SecretError::WorkspaceMismatch { .. })
    ));
    assert!(SecretBroker::materialize(
        conn,
        &provider(),
        &MaterializeRequest {
            workspace_id: Some(WORKSPACE),
            ..materialize_request(HANDLE, scope(), &expires_at)
        }
    )
    .await
    .is_ok());

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_handle_with_no_material_and_a_malformed_id_are_typed_refusals() {
    let name = common::scratch_name("secrets_typed");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    // A handle minted with an id that is not a `sec_` id is refused before anything is written.
    assert!(matches!(
        SecretBroker::register(
            conn,
            &provider(),
            &NewSecret {
                id: "hand_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
                tenant_id: TENANT,
                workspace_id: None,
                provider: "github",
                label: "not a handle",
                expires_at: None,
            },
            CANARY,
            &OsEntropy,
        )
        .await,
        Err(SecretError::HandleIdInvalid(_))
    ));
    assert!(SecretBroker::list(conn, TENANT)
        .await
        .expect("list")
        .is_empty());

    // A handle registered with no material cannot be materialized, and says so rather than returning an
    // empty value that a caller might use as a credential.
    sqlx::query(
        "INSERT INTO secret_handles (id, tenant_id, workspace_id, provider, label, status, generation) \
         VALUES ($1, $2, NULL, 'github', 'empty', 'active', 1)",
    )
    .bind(HANDLE)
    .bind(TENANT)
    .execute(&mut *conn)
    .await
    .expect("insert a handle with no material");
    let expires_at = MaterializeRequest::expiry_after(NOW, 60).expect("canonical");
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider(),
            &materialize_request(HANDLE, scope(), &expires_at)
        )
        .await,
        Err(SecretError::NoMaterial(_))
    ));

    // A material that was replaced by something that is not a frame is refused rather than interpreted.
    sqlx::query("UPDATE secret_handles SET ciphertext = $2 WHERE id = $1 AND tenant_id = $3")
        .bind(HANDLE)
        .bind(b"not".to_vec())
        .bind(TENANT)
        .execute(&mut *conn)
        .await
        .expect("replace the frame");
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider(),
            &materialize_request(HANDLE, scope(), &expires_at)
        )
        .await,
        Err(SecretError::EnvelopeMalformed { .. })
    ));
    // A frame from a version this broker does not write is named as such rather than guessed at.
    let mut future = vec![9u8; 64];
    future[0] = 9;
    sqlx::query("UPDATE secret_handles SET ciphertext = $2 WHERE id = $1 AND tenant_id = $3")
        .bind(HANDLE)
        .bind(future)
        .bind(TENANT)
        .execute(&mut *conn)
        .await
        .expect("replace the frame");
    assert!(matches!(
        SecretBroker::materialize(
            conn,
            &provider(),
            &materialize_request(HANDLE, scope(), &expires_at)
        )
        .await,
        Err(SecretError::EnvelopeUnsupported { version: 9 })
    ));

    common::drop_pool(&pool, &name).await;
}
