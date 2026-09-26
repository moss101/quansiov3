//! APP-002 identity, onboarding and settings-merge tests.

use quansio_server::control::identity::{merge_settings, platform_defaults, IdentityStore};
use quansio_server::control::schema;
use serde_json::json;

mod common;

#[test]
fn security_settings_merge_is_deterministic_and_most_restrictive() {
    let platform = platform_defaults();
    let tenant = json!({ "telemetry": true, "retention_days": 90, "theme": "dark" });
    let workspace = json!({ "telemetry": false, "clipboard_to_model": true, "retention_days": 30 });
    let user = json!({ "telemetry": true, "theme": "light", "retention_days": 365 });
    let first = merge_settings(&[
        platform.clone(),
        tenant.clone(),
        workspace.clone(),
        user.clone(),
    ]);
    let second = merge_settings(&[platform, tenant, workspace, user]);
    assert_eq!(first, second);
    assert_eq!(first["telemetry"], false, "deny wins");
    assert_eq!(
        first["clipboard_to_model"], false,
        "platform deny plus workspace allow still deny"
    );
    assert_eq!(first["unsigned_updates"], false);
    assert_eq!(first["retention_days"], 30, "minimum retention wins");
    assert_eq!(first["theme"], "light", "non-security keys override");
}

#[tokio::test]
async fn a_fresh_user_is_onboarded_without_a_config_file() {
    let name = common::scratch_name("id_onboard");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    let store = IdentityStore::new(pool.clone());
    let onboarded = store
        .onboard_email("ada@example.com", "Ada", "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA")
        .await
        .expect("onboard");
    assert!(onboarded.user_id.starts_with("usr_"));
    assert!(onboarded.tenant_id.starts_with("tn_"));
    assert!(onboarded.workspace_id.starts_with("ws_"));

    let methods: serde_json::Value =
        sqlx::query_scalar("SELECT auth_methods FROM users WHERE id = $1")
            .bind(&onboarded.user_id)
            .fetch_one(&pool)
            .await
            .expect("auth methods");
    let dumped = methods.to_string();
    assert!(dumped.contains("email_magic_link"));
    assert!(dumped.contains("sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"));
    assert!(!dumped.contains("sk-"), "raw provider secret leaked");
    assert!(!dumped.to_lowercase().contains("password"));

    let personal: bool = sqlx::query_scalar("SELECT personal FROM tenants WHERE id = $1")
        .bind(&onboarded.tenant_id)
        .fetch_one(&pool)
        .await
        .expect("personal");
    assert!(personal);

    let effective = store
        .effective_for_workspace(
            &onboarded.tenant_id,
            &onboarded.workspace_id,
            &onboarded.user_id,
        )
        .await
        .expect("settings");
    assert_eq!(effective["unsigned_updates"], false);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn onboarding_refuses_a_raw_secret() {
    let name = common::scratch_name("id_secret");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    let store = IdentityStore::new(pool.clone());
    let error = store
        .onboard_email("x@example.com", "X", "sk-live-not-a-handle")
        .await
        .expect_err("raw secret");
    assert!(error.to_string().contains("sec_"), "{error}");
    common::drop_pool(&pool, &name).await;
}

// ---------------------------------------------------------------------------------------
// Sessions and sign-in (DOMAIN.md §2), driven through the public router.
// ---------------------------------------------------------------------------------------

use std::sync::Arc;

use axum::http::{Request, StatusCode};
use chrono::{Duration as ChronoDuration, Utc};
use quansio_server::api::auth::RecordingDelivery;
use quansio_server::api::{ApiState, ScopeTrust};
use quansio_server::control::identity::auth::{
    mint_access_token, verify_access_token, AccessClaims, SigningKey,
};
use quansio_server::control::identity::oauth::{OAuthExchange, ProviderIdentity, ReadyProvider};
use quansio_server::control::identity::AuthError;
use serde_json::Value;
use tower::ServiceExt;

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn post_json(path: &str, body: Value, bearer: Option<&str>) -> Request<axum::body::Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    builder
        .body(axum::body::Body::from(body.to_string()))
        .expect("request")
}

fn get_bearer(path: &str, bearer: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {bearer}"))
        .body(axum::body::Body::empty())
        .expect("request")
}

async fn session_state(
    prefix: &str,
) -> Option<(ApiState, Arc<RecordingDelivery>, sqlx::PgPool, String)> {
    let name = common::scratch_name(prefix);
    let pool = common::fresh_database(&name).await?;
    schema::migrate(&pool).await.expect("migrate");
    let delivery = Arc::new(RecordingDelivery::default());
    let state = ApiState::new(pool.clone())
        .expect("api state")
        .with_scope_trust(ScopeTrust::SessionOnly)
        .with_magic_link_delivery(delivery.clone());
    Some((state, delivery, pool, name))
}

/// Sign `email` in through the magic link and return the session document.
async fn sign_in(router: &axum::Router, delivery: &RecordingDelivery, email: &str) -> Value {
    let started = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/magic-link/start",
            json!({ "email": email, "display_name": "Ada" }),
            None,
        ))
        .await
        .expect("start");
    assert_eq!(started.status(), StatusCode::OK);
    let challenge = delivery
        .delivered()
        .into_iter()
        .rev()
        .find(|c| c.email == email.to_ascii_lowercase())
        .expect("a delivered magic link");
    let completed = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/magic-link/complete",
            json!({ "token": challenge.token }),
            None,
        ))
        .await
        .expect("complete");
    let status = completed.status();
    let body = json_body(completed).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn a_fresh_user_signs_up_by_magic_link_and_gets_a_governed_workspace() {
    let Some((state, delivery, pool, name)) = session_state("auth_signup").await else {
        common::blocked_marker();
        return;
    };
    let router = quansio_server::api::router(state);

    let session = sign_in(&router, &delivery, "Ada@Example.com").await;
    assert_eq!(session["onboarded"], true, "{session}");
    assert!(session["user_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("usr_"));
    assert!(session["tenant_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("tn_"));
    assert!(session["workspace_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("ws_"));
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    assert_eq!(access.matches('.').count(), 2, "compact JWS");
    assert!(session["refresh_token"]
        .as_str()
        .unwrap_or("")
        .starts_with("qrt_"));

    // The magic-link token is single use.
    let token = delivery.delivered().pop().expect("challenge").token;
    let reused = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/magic-link/complete",
            json!({ "token": token }),
            None,
        ))
        .await
        .expect("reuse");
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(reused).await["code"], "AUTH_INVALID_TOKEN");

    // /v1/me: the user, the personal tenant, the default workspace with effective settings.
    let me = router
        .clone()
        .oneshot(get_bearer("/v1/me", &access))
        .await
        .expect("me");
    assert_eq!(me.status(), StatusCode::OK);
    let me = json_body(me).await;
    assert_eq!(me["user"]["primary_email"], "ada@example.com");
    assert_eq!(me["tenants"][0]["personal"], true);
    assert_eq!(me["tenants"][0]["role"], "owner");
    assert_eq!(me["workspaces"][0]["role"], "admin");
    assert_eq!(
        me["workspaces"][0]["effective_settings"]["unsigned_updates"],
        false
    );
    assert_eq!(
        me["workspaces"][0]["effective_settings"]["telemetry"],
        false
    );
    assert_eq!(me["auth_methods_offered"]["email_magic_link"], true);
    let methods = me["user"]["auth_methods"].to_string();
    assert!(
        methods.contains("email_magic_link") && methods.contains("oauth_google"),
        "{methods}"
    );
    assert!(
        !methods.contains("qml_"),
        "the magic-link token must not be recorded"
    );

    // A second sign-in of the same address is the same user, not a second tenant.
    let again = sign_in(&router, &delivery, "ada@example.com").await;
    assert_eq!(again["onboarded"], false, "{again}");
    assert_eq!(again["user_id"], session["user_id"]);
    assert_eq!(again["tenant_id"], session["tenant_id"]);

    common::drop_pool(&pool, &name).await;
}

#[test]
fn access_tokens_are_es256_and_expire() {
    let key = SigningKey::ephemeral().expect("key");
    let now = Utc::now();
    let claims = AccessClaims {
        sub: "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string(),
        tid: "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string(),
        sid: "ses_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string(),
        iat: now.timestamp(),
        exp: (now + ChronoDuration::minutes(15)).timestamp(),
    };
    let token = mint_access_token(&key, &claims).expect("mint");
    let header: Value =
        serde_json::from_slice(&base64_url_decode(token.split('.').next().expect("header")))
            .expect("header json");
    assert_eq!(header["alg"], "ES256");
    assert_eq!(header["kid"], key.kid());
    assert_eq!(
        verify_access_token(&key, &token, now).expect("verify"),
        claims
    );

    // Tampered claims do not verify; another key does not verify; expiry is enforced.
    let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
    parts[1] = base64_url_encode(
        br#"{"sub":"usr_x","tid":"tn_x","sid":"ses_x","iat":0,"exp":9999999999}"#,
    );
    let tampered = parts.join(".");
    assert!(matches!(
        verify_access_token(&key, &tampered, now),
        Err(AuthError::InvalidToken(_))
    ));
    let other = SigningKey::ephemeral().expect("other key");
    assert!(matches!(
        verify_access_token(&other, &token, now),
        Err(AuthError::InvalidToken(_))
    ));
    assert!(matches!(
        verify_access_token(&key, &token, now + ChronoDuration::minutes(16)),
        Err(AuthError::Expired(_))
    ));
    assert!(matches!(
        verify_access_token(&key, "not.a.jws", now),
        Err(AuthError::InvalidToken(_))
    ));
}

fn base64_url_decode(value: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .expect("base64url")
}

fn base64_url_encode(value: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

#[tokio::test]
async fn refresh_rotates_and_a_replayed_token_revokes_the_family() {
    let Some((state, delivery, pool, name)) = session_state("auth_refresh").await else {
        common::blocked_marker();
        return;
    };
    let router = quansio_server::api::router(state);
    let first = sign_in(&router, &delivery, "bob@example.com").await;
    let refresh_1 = first["refresh_token"]
        .as_str()
        .expect("refresh")
        .to_string();

    let rotated = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/refresh",
            json!({ "refresh_token": refresh_1 }),
            None,
        ))
        .await
        .expect("refresh");
    assert_eq!(rotated.status(), StatusCode::OK);
    let second = json_body(rotated).await;
    let refresh_2 = second["refresh_token"]
        .as_str()
        .expect("refresh 2")
        .to_string();
    assert_ne!(refresh_2, refresh_1);
    assert_ne!(second["session_id"], first["session_id"]);
    let access_2 = second["access_token"]
        .as_str()
        .expect("access 2")
        .to_string();
    assert_eq!(
        router
            .clone()
            .oneshot(get_bearer("/v1/me", &access_2))
            .await
            .expect("me")
            .status(),
        StatusCode::OK
    );

    // Replaying the retired token is theft by definition: the whole family is revoked.
    let replay = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/refresh",
            json!({ "refresh_token": refresh_1 }),
            None,
        ))
        .await
        .expect("replay");
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(replay).await["code"], "AUTH_INVALID_TOKEN");
    let after = router
        .clone()
        .oneshot(get_bearer("/v1/me", &access_2))
        .await
        .expect("me");
    assert_eq!(
        after.status(),
        StatusCode::UNAUTHORIZED,
        "the descendant session is revoked too"
    );
    let after_refresh = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/refresh",
            json!({ "refresh_token": refresh_2 }),
            None,
        ))
        .await
        .expect("refresh after revoke");
    assert_eq!(after_refresh.status(), StatusCode::UNAUTHORIZED);

    // An unknown token is refused without revealing anything.
    let unknown = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/refresh",
            json!({ "refresh_token": "qrt_nope" }),
            None,
        ))
        .await
        .expect("unknown");
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn sign_out_revokes_and_session_only_refuses_trusted_headers() {
    let Some((state, delivery, pool, name)) = session_state("auth_signout").await else {
        common::blocked_marker();
        return;
    };
    let router = quansio_server::api::router(state);
    let session = sign_in(&router, &delivery, "cy@example.com").await;
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    let tenant = session["tenant_id"].as_str().expect("tenant").to_string();

    // Headers alone do not establish a scope on a session-only state.
    let headers_only = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/me")
                .header("x-quansio-tenant", &tenant)
                .header("x-quansio-user", session["user_id"].as_str().expect("user"))
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("headers only");
    assert_eq!(headers_only.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(headers_only).await["code"], "AUTH_REQUIRED");

    // A garbage bearer is AUTH_INVALID_TOKEN, distinct from a missing one.
    let garbage = router
        .clone()
        .oneshot(get_bearer("/v1/me", "garbage"))
        .await
        .expect("garbage");
    assert_eq!(garbage.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(garbage).await["code"], "AUTH_INVALID_TOKEN");

    let signed_out = router
        .clone()
        .oneshot(post_json("/v1/auth/sign-out", json!({}), Some(&access)))
        .await
        .expect("sign out");
    assert_eq!(signed_out.status(), StatusCode::OK);
    let after = router
        .clone()
        .oneshot(get_bearer("/v1/me", &access))
        .await
        .expect("me");
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn identity_commands_manage_workspaces_and_settings_with_secrets_as_handles() {
    let Some((state, delivery, pool, name)) = session_state("auth_commands").await else {
        common::blocked_marker();
        return;
    };
    let router = quansio_server::api::router(state);
    let session = sign_in(&router, &delivery, "dee@example.com").await;
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    let default_workspace = session["workspace_id"].as_str().expect("ws").to_string();

    let created = router
        .clone()
        .oneshot(post_json(
            "/v1/commands/CreateWorkspace",
            json!({ "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAA1", "params": { "name": "Research" } }),
            Some(&access),
        ))
        .await
        .expect("create");
    let status = created.status();
    let created = json_body(created).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let research = created["result"]["workspace_id"]
        .as_str()
        .expect("id")
        .to_string();
    assert!(research.starts_with("ws_") && research != default_workspace);

    // Execution target choice and model/provider setup are settings; a change with a
    // restart consequence reports it; a raw secret is refused before anything is stored.
    let updated = router
        .clone()
        .oneshot(post_json(
            "/v1/commands/UpdateWorkspace",
            json!({ "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAA2", "params": {
                "workspace_id": research,
                "settings": {
                    "execution_target": "cloud",
                    "model": { "primary": "anthropic-sonnet" },
                    "providers": { "anthropic": { "credential_handle": "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" } }
                }
            }}),
            Some(&access),
        ))
        .await
        .expect("update");
    let status = updated.status();
    let updated = json_body(updated).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        updated["result"]["effective_settings"]["execution_target"],
        "cloud"
    );
    let effects = updated["result"]["effects"].to_string();
    assert!(effects.contains("restart"), "{effects}");
    assert!(
        effects.contains("execution target") && effects.contains("provider credentials"),
        "{effects}"
    );

    let raw_secret = router
        .clone()
        .oneshot(post_json(
            "/v1/commands/UpdateWorkspace",
            json!({ "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAA3", "params": {
                "workspace_id": research,
                "settings": { "providers": { "openai": { "credential_handle": "sk-live-abcdef" } } }
            }}),
            Some(&access),
        ))
        .await
        .expect("raw secret");
    assert_eq!(raw_secret.status(), StatusCode::BAD_REQUEST);
    let refused = json_body(raw_secret).await;
    assert!(
        refused["message"].as_str().unwrap_or("").contains("sec_"),
        "{refused}"
    );
    let stored: Value = sqlx::query_scalar("SELECT settings FROM workspaces WHERE id = $1")
        .bind(&research)
        .fetch_one(&pool)
        .await
        .expect("settings");
    assert!(
        !stored.to_string().contains("sk-live"),
        "raw secret must never be stored"
    );

    // Settings precedence: the user layer cannot widen a security control the workspace denies.
    let user_layer = router
        .clone()
        .oneshot(post_json(
            "/v1/commands/UpdateUserSettings",
            json!({ "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAA4", "params": {
                "workspace_id": research,
                "settings": { "telemetry": true, "theme": "light", "retention_days": 400 }
            }}),
            Some(&access),
        ))
        .await
        .expect("user settings");
    let status = user_layer.status();
    let user_layer = json_body(user_layer).await;
    assert_eq!(status, StatusCode::OK, "{user_layer}");
    let effective = &user_layer["result"]["effective_settings"];
    assert_eq!(
        effective["telemetry"], false,
        "deny wins over the user layer"
    );
    assert_eq!(effective["retention_days"], 365, "minimum wins");
    assert_eq!(
        effective["theme"], "light",
        "non-security keys follow the user"
    );

    // Without an acting user the identity commands refuse (trusted-header mode, no user header).
    let no_user_state = ApiState::new(pool.clone()).expect("state");
    let no_user_router = quansio_server::api::router(no_user_state);
    let refused = no_user_router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/commands/CreateWorkspace")
                .header("content-type", "application/json")
                .header("x-quansio-tenant", session["tenant_id"].as_str().expect("tenant"))
                .body(axum::body::Body::from(
                    json!({ "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAA5", "params": { "name": "x" } }).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("no user");
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    common::drop_pool(&pool, &name).await;
}

/// A provider double: the exchange never leaves the process.
struct StubExchange;

#[async_trait::async_trait]
impl OAuthExchange for StubExchange {
    async fn exchange(
        &self,
        provider: &ReadyProvider,
        client_secret: &str,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<ProviderIdentity, AuthError> {
        assert_eq!(provider.name, "github");
        assert_eq!(client_secret, "stub-secret");
        assert!(!code_verifier.is_empty());
        assert_eq!(redirect_uri, "quansio://auth/callback");
        if code != "good-code" {
            return Err(AuthError::InvalidToken("bad code".to_string()));
        }
        Ok(ProviderIdentity {
            subject: "12345".to_string(),
            email: "eve@example.com".to_string(),
            display_name: "Eve".to_string(),
        })
    }
}

#[tokio::test]
async fn oauth_signs_in_with_pkce_and_single_use_state() {
    let Some((state, _delivery, pool, name)) = session_state("auth_oauth").await else {
        common::blocked_marker();
        return;
    };
    std::env::set_var("QUANSIO_OAUTH_GITHUB_CLIENT_ID", "stub-client");
    std::env::set_var("QUANSIO_SECRET_OAUTH_GITHUB", "stub-secret");
    let router = quansio_server::api::router(state.with_oauth_exchange(Arc::new(StubExchange)));

    let unconfigured = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/oauth/google/start?redirect_uri=quansio://auth/callback")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("google");
    assert_eq!(
        json_body(unconfigured).await["code"],
        "PROVIDER_UNAVAILABLE"
    );

    let started = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/oauth/github/start?redirect_uri=quansio://auth/callback")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("start");
    assert_eq!(started.status(), StatusCode::OK);
    let started = json_body(started).await;
    let url = started["authorization_url"].as_str().expect("url");
    assert!(url.starts_with("https://github.com/login/oauth/authorize?"));
    assert!(url.contains("code_challenge_method=S256") && url.contains("client_id=stub-client"));
    assert!(!url.contains("stub-secret"));
    let oauth_state = started["state"].as_str().expect("state").to_string();

    let completed = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/oauth/github/complete",
            json!({ "code": "good-code", "state": oauth_state, "redirect_uri": "quansio://auth/callback" }),
            None,
        ))
        .await
        .expect("complete");
    let status = completed.status();
    let session = json_body(completed).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(session["onboarded"], true);
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    let me = json_body(
        router
            .clone()
            .oneshot(get_bearer("/v1/me", &access))
            .await
            .expect("me"),
    )
    .await;
    assert_eq!(me["user"]["primary_email"], "eve@example.com");
    assert!(me["user"]["auth_methods"]
        .to_string()
        .contains("oauth_github"));

    let reused = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/oauth/github/complete",
            json!({ "code": "good-code", "state": oauth_state, "redirect_uri": "quansio://auth/callback" }),
            None,
        ))
        .await
        .expect("reuse");
    assert_eq!(
        reused.status(),
        StatusCode::UNAUTHORIZED,
        "state is single use"
    );
    common::drop_pool(&pool, &name).await;
}

/// A pool whose every connection acts as the application role, so row-level security is
/// enforced exactly as it is for the deployed server.
async fn app_role_pool(url: &str) -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET ROLE quansio_app").execute(conn).await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .expect("app-role pool")
}

#[tokio::test]
async fn the_whole_sign_in_flow_works_as_the_application_role_under_rls() {
    let name = common::scratch_name("auth_rls");
    let Some(admin) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&admin).await.expect("migrate");
    let pool = app_role_pool(&common::scratch_url(&name)).await;
    let delivery = Arc::new(RecordingDelivery::default());
    let state = ApiState::new(pool.clone())
        .expect("api state")
        .with_scope_trust(ScopeTrust::SessionOnly)
        .with_magic_link_delivery(delivery.clone());
    let router = quansio_server::api::router(state);

    // Onboarding, session issuance, bearer resolution, /v1/me, refresh and sign-out: all as
    // quansio_app, where every table's policy is in force.
    let session = sign_in(&router, &delivery, "rls@example.com").await;
    assert_eq!(session["onboarded"], true, "{session}");
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    let me = router
        .clone()
        .oneshot(get_bearer("/v1/me", &access))
        .await
        .expect("me");
    assert_eq!(me.status(), StatusCode::OK);
    let refreshed = router
        .clone()
        .oneshot(post_json(
            "/v1/auth/refresh",
            json!({ "refresh_token": session["refresh_token"] }),
            None,
        ))
        .await
        .expect("refresh");
    assert_eq!(refreshed.status(), StatusCode::OK);
    let signed_out = router
        .clone()
        .oneshot(post_json("/v1/auth/sign-out", json!({}), Some(&access)))
        .await
        .expect("sign out");
    assert_eq!(signed_out.status(), StatusCode::OK);

    // A returning user signs in again as the application role: the account is found, not
    // onboarded a second time (the users policy hides it until the flow names it).
    let again = sign_in(&router, &delivery, "rls@example.com").await;
    assert_eq!(again["onboarded"], false, "{again}");
    assert_eq!(again["user_id"], session["user_id"], "{again}");
    assert_eq!(again["tenant_id"], session["tenant_id"], "{again}");

    // An administrator adds an account that already exists elsewhere (it has its own personal
    // tenant): as the application role the account is found, not provisioned a second time.
    let other = sign_in(&router, &delivery, "rls-other@example.com").await;
    let again_access = again["access_token"].as_str().expect("access").to_string();
    let invited = router
        .clone()
        .oneshot(post_json(
            "/v1/commands/InviteMember",
            json!({ "command_id": "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAB1", "params": { "email": "rls-other@example.com", "role": "member" } }),
            Some(&again_access),
        ))
        .await
        .expect("invite");
    let status = invited.status();
    let invited = json_body(invited).await;
    assert_eq!(status, StatusCode::OK, "{invited}");
    assert_eq!(
        invited["result"]["user"]["id"], other["user_id"],
        "{invited}"
    );
    assert_eq!(
        invited["result"]["status"], "active",
        "an existing account joins directly: {invited}"
    );

    // The pre-authentication tables are invisible to the application role without the gate.
    let mut tx = pool.begin().await.expect("begin");
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    assert_eq!(
        visible, 0,
        "sessions are invisible without the auth gate or a tenant context"
    );
    let challenges: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_challenges")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    assert_eq!(challenges, 0);
    sqlx::query("SELECT set_config('quansio.auth_flow', 'on', true)")
        .execute(&mut *tx)
        .await
        .expect("gate");
    let gated: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    assert!(
        gated >= 2,
        "the gate reveals the sessions the flow created: {gated}"
    );
    tx.rollback().await.ok();
    pool.close().await;
    common::drop_pool(&admin, &name).await;
}

/// Read Server-Sent Events until one named `event: error` arrives; its data, or None at timeout.
async fn sse_error(
    response: axum::response::Response,
    within: std::time::Duration,
) -> Option<Value> {
    use futures_util::StreamExt;
    let mut body = response.into_body().into_data_stream();
    let mut buffer = String::new();
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Some(start) = buffer.find("event: error\n") {
            let rest = &buffer[start..];
            if let Some(data) = rest.lines().find_map(|line| line.strip_prefix("data: ")) {
                return serde_json::from_str(data).ok();
            }
        }
        let chunk = tokio::time::timeout_at(deadline, body.next())
            .await
            .ok()??;
        buffer.push_str(&String::from_utf8_lossy(&chunk.ok()?));
    }
}

/// An open stream ends when its session does (CORE-009 regression found by QA-008): streams
/// were authenticated only when they opened, so a revoked session kept receiving the tenant's
/// events for as long as the connection stayed up.
#[tokio::test]
async fn an_open_stream_closes_when_its_session_is_revoked() {
    let Some((state, delivery, pool, name)) = session_state("auth_stream_revoked").await else {
        common::blocked_marker();
        return;
    };
    let router = quansio_server::api::router(state);
    let session = sign_in(&router, &delivery, "stream@example.com").await;
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    let workspace = session["workspace_id"]
        .as_str()
        .expect("workspace")
        .to_string();
    let path = format!("/v1/stream/events?channel=workspace%3A{workspace}");

    // Positive control: with the session live the stream stays open past a re-check.
    let open = router
        .clone()
        .oneshot(get_bearer(&path, &access))
        .await
        .expect("stream");
    assert_eq!(open.status(), StatusCode::OK);
    let quiet = sse_error(
        open,
        quansio_server::api::stream::REVALIDATE_EVERY + std::time::Duration::from_secs(1),
    )
    .await;
    assert!(
        quiet.is_none(),
        "a live session's stream was closed: {quiet:?}"
    );

    // Revoke while a stream is open: it ends with a typed refusal within one re-check.
    let open = router
        .clone()
        .oneshot(get_bearer(&path, &access))
        .await
        .expect("stream");
    assert_eq!(open.status(), StatusCode::OK);
    let signed_out = router
        .clone()
        .oneshot(post_json("/v1/auth/sign-out", json!({}), Some(&access)))
        .await
        .expect("sign-out");
    assert_eq!(signed_out.status(), StatusCode::OK);
    let refusal = sse_error(open, quansio_server::api::stream::REVALIDATE_EVERY * 3)
        .await
        .expect("the stream of a revoked session ends with an error event");
    assert_eq!(refusal["code"], "AUTH_INVALID_TOKEN", "{refusal}");

    // And a new stream with the revoked token is refused outright.
    let again = router
        .clone()
        .oneshot(get_bearer(&path, &access))
        .await
        .expect("stream");
    assert_eq!(again.status(), StatusCode::UNAUTHORIZED);

    common::drop_pool(&pool, &name).await;
}
