//! EXEC-011 broker tests: OAuth begin/complete with handles only, lifecycle
//! transitions, state-digest replay refusal.
//!
//! The sandbox runs are the real boundary (`QUANSIO_TEST_CONNECTOR_GITHUB=1` etc.);
//! these cases drive the broker over real PostgreSQL with a fixture exchange, which
//! is the same code the sandbox run executes.

use quansio_server::control::connectors::{
    ConnectorBroker, ConnectorError, ConnectorState, IssuedToken, TokenExchange,
};
use quansio_server::control::schema;

mod common;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CN011";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0CN011";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CN011";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0CN011";

/// Fixture exchange: the provider stand-in. It yields handle-shaped material so
/// nothing here ever holds raw token bytes — the real exchange's opaque output is
/// written straight into the secret-store by the integration layer, which is the
/// piece the sandbox exercises.
struct FixtureExchange;

impl TokenExchange for FixtureExchange {
    fn exchange(&self, _connector: &str, _code: &str, _state: &str) -> Result<IssuedToken, String> {
        Ok(IssuedToken {
            access_token: "sec_01J8Z3K6F1N8VQ2X5W9Y0GHTOKEN".to_string(),
            refresh_token: Some("sec_01J8Z3K6F1N8VQ2X5W9Y0GHREFRESH".to_string()),
            expires_in: Some(3600),
        })
    }
}

async fn prepare(prefix: &str) -> Option<(sqlx::PgPool, ConnectorBroker)> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    Some((pool.clone(), ConnectorBroker::new(pool, TENANT)))
}

#[tokio::test]
async fn begin_and_complete_auth_stores_a_handle_and_never_token_material() {
    let Some((pool, broker)) = prepare("cn_broker_auth").await else {
        blocked_marker();
        return;
    };
    let start = broker
        .begin_auth(
            "github",
            "GitHub workspace",
            "https://github.com/login/oauth/authorize",
            "state-0123456789abcdef",
            Some(WORKSPACE),
        )
        .await
        .expect("begin");
    assert!(start.authorize_url.starts_with("https://"));
    assert!(start.state.len() >= 16);

    let exchange = FixtureExchange
        .exchange("github", "code", &start.state)
        .expect("exchange");
    store_handle(&pool, &exchange.access_token, "github").await;
    let (instance, handle) = broker
        .complete_auth(
            &start.instance_id,
            &start.state,
            &exchange,
            &exchange.access_token,
        )
        .await
        .expect("complete");
    assert_eq!(instance, start.instance_id);
    assert!(handle.starts_with("sec_"));

    let (metadata, connector_id, status, stored) = broker.load_async(&instance).await.expect("row");
    assert_eq!(connector_id, "github");
    assert_eq!(status, "connected");
    assert_eq!(stored.as_deref(), Some(handle.as_str()));
    // The state digest was consumed; the row holds neither the state nor any token.
    assert!(metadata.get("oauth_state_digest").is_none());
    let row_json = serde_json::to_string(&metadata).unwrap();
    assert!(
        !row_json.contains("token"),
        "row must not carry token material"
    );

    // A replayed state is refused: the digest was cleared on completion.
    let replay = broker
        .complete_auth(&instance, &start.state, &exchange, "sec_another")
        .await;
    assert!(matches!(replay, Err(ConnectorError::StateMismatch { .. })));
    drop_pool(&pool, &cn_scratch_name("cn_broker_auth")).await;
}

fn cn_scratch_name(prefix: &str) -> String {
    format!("quansio_{prefix}_{}", std::process::id())
}

/// The integration-layer step the real deployment runs between exchange and broker
/// completion: the secret-store owner receives the token material and returns/records
/// the handle. The broker only ever sees the handle.
async fn store_handle(pool: &sqlx::PgPool, handle: &str, provider: &str) {
    let mut tx = pool.begin().await.expect("begin");
    schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("context");
    sqlx::query(
        "INSERT INTO secret_handles (id, tenant_id, provider, label) \
         VALUES ($1, $2, $3, 'oauth token') ON CONFLICT (id) DO NOTHING",
    )
    .bind(handle)
    .bind(TENANT)
    .bind(provider)
    .execute(&mut *tx)
    .await
    .expect("insert handle");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn lifecycle_degrades_and_revocation_clears_the_handle() {
    let Some((pool, broker)) = prepare("cn_broker_lifecycle").await else {
        blocked_marker();
        return;
    };
    let start = broker
        .begin_auth(
            "slack",
            "Slack",
            "https://slack.com/oauth/authorize",
            "state-fedcba9876543210",
            None,
        )
        .await
        .expect("begin");
    let exchange = FixtureExchange
        .exchange("slack", "code", &start.state)
        .expect("exchange");
    store_handle(&pool, &exchange.access_token, "slack").await;
    broker
        .complete_auth(
            &start.instance_id,
            &start.state,
            &exchange,
            &exchange.access_token,
        )
        .await
        .expect("complete");

    broker
        .transition(&start.instance_id, ConnectorState::Degraded)
        .await
        .expect("degrade");
    let (_, _, status, handle) = broker.load_async(&start.instance_id).await.expect("row");
    assert_eq!(status, "degraded");
    assert!(
        handle.is_some(),
        "degrading keeps the credential for recovery"
    );

    broker
        .transition(&start.instance_id, ConnectorState::Revoked)
        .await
        .expect("revoke");
    let (_, _, status, handle) = broker.load_async(&start.instance_id).await.expect("row");
    assert_eq!(status, "revoked");
    assert!(
        handle.is_none(),
        "revocation must clear the credential handle"
    );
    assert!(!ConnectorState::Revoked.dispatchable());

    // Revocation is terminal for this row; re-authentication is a new begin_auth.
    let resurrect = broker
        .transition(&start.instance_id, ConnectorState::Connected)
        .await;
    assert!(matches!(
        resurrect,
        Err(ConnectorError::IllegalTransition { .. })
    ));
    drop_pool(&pool, &cn_scratch_name("cn_broker_lifecycle")).await;
}

#[tokio::test]
async fn raw_token_material_and_foreign_state_are_refused() {
    let Some((pool, broker)) = prepare("cn_broker_refuse").await else {
        blocked_marker();
        return;
    };
    let start = broker
        .begin_auth(
            "github",
            "GitHub",
            "https://github.com/login/oauth/authorize",
            "state-aaaabbbbccccdddd",
            None,
        )
        .await
        .expect("begin");
    let exchange = FixtureExchange
        .exchange("github", "code", &start.state)
        .expect("exchange");

    // Raw material instead of a handle: refused before anything is written.
    let raw = broker
        .complete_auth(
            &start.instance_id,
            &start.state,
            &exchange,
            "gho_realTokenBytes",
        )
        .await;
    assert!(matches!(raw, Err(ConnectorError::RawToken)));
    let (_, _, _, handle) = broker.load_async(&start.instance_id).await.expect("row");
    assert!(
        handle.is_none(),
        "a refused completion must not store anything"
    );

    // The happy path after the refusal: store the handle, then complete.
    store_handle(&pool, &exchange.access_token, "github").await;
    broker
        .complete_auth(
            &start.instance_id,
            &start.state,
            &exchange,
            &exchange.access_token,
        )
        .await
        .expect("complete after refusal");

    // A state that was never issued: refused.
    let foreign = broker
        .complete_auth(
            &start.instance_id,
            "state-9999888877776666",
            &exchange,
            &exchange.access_token,
        )
        .await;
    assert!(matches!(foreign, Err(ConnectorError::StateMismatch { .. })));
    assert_eq!(foreign.expect_err("code").code(), "CONFLICT_STATE");
    drop_pool(&pool, &cn_scratch_name("cn_broker_refuse")).await;
}

#[test]
fn sandbox_boundary_is_reported_when_absent() {
    if std::env::var("QUANSIO_TEST_CONNECTOR_GITHUB")
        .ok()
        .as_deref()
        != Some("1")
    {
        eprintln!(
            "BLOCKED_EXTERNAL: QUANSIO_TEST_CONNECTOR_GITHUB=1 (and the other GA connector \
             sandbox flags) are not set; in-sandbox conformance runs are not executing"
        );
    }
}
