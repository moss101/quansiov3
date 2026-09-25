//! OPS-001: enterprise identity end to end: SSO, SCIM, service principals, session revocation.
//!
//! The in-test OpenID provider is real HTTP on loopback:
//! - discovery;
//! - a JWKS holding an ES256 key and an RS256 key. The RS256 key is
//!   `tests/fixtures/oidc-test-idp-rsa.der`, a throwaway key that belongs to this test IdP only;
//! - the authorization-code flow with PKCE S256;
//! - signed id_tokens.
//!
//! The identity surface runs as the application role (`quansio_app`), so every row-level-security
//! policy of the identity tables is in force:
//! - sign-in and SSO;
//! - SCIM;
//! - bearer and principal authentication;
//! - the session and SSO/principal read endpoints.
//!
//! `/v1/commands/*` runs on the superuser pool, as every other API test does. The command log
//! itself is not yet RLS-clean for the application role, which is a separate finding.
//!
//! The live proof against a real IdP (`a_real_idp_issues_id_tokens_the_verifier_accepts`) reads:
//! - `QUANSIO_TEST_OIDC_ISSUER`: a sandbox realm's issuer (https);
//! - `QUANSIO_TEST_OIDC_CLIENT_ID` and `QUANSIO_TEST_OIDC_CLIENT_SECRET`: a confidential client
//!   with the resource-owner password grant enabled (sandbox only);
//! - `QUANSIO_TEST_OIDC_USERNAME` and `QUANSIO_TEST_OIDC_PASSWORD`: a test user of that realm.
//!
//! Without them it prints `BLOCKED_EXTERNAL` and returns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_server::api::auth::RecordingDelivery;
use quansio_server::api::{ApiState, ScopeTrust};
use quansio_server::control::identity::enterprise::jwt::{self, Expectations, Jwks};
use quansio_server::control::identity::enterprise::LIVE_OIDC_ENV;
use quansio_server::control::identity::EnterpriseConfig;
use quansio_server::control::schema;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{
    EcdsaKeyPair, KeyPair, RsaKeyPair, RsaPublicKeyComponents, ECDSA_P256_SHA256_FIXED_SIGNING,
    RSA_PKCS1_SHA256,
};
use serde_json::{json, Map, Value};
use tower::ServiceExt;

mod common;

const CLIENT_ID: &str = "quansio-ops001";
const CLIENT_SECRET: &str = "ops001-in-test-idp-client-secret";
const SECRET_HANDLE: &str = "sso/ops001";
const REDIRECT: &str = "http://127.0.0.1:53123/sso/callback";
const RSA_KEY: &[u8] = include_bytes!("fixtures/oidc-test-idp-rsa.der");

// ------------------------------------------------------------------ the in-test OpenID provider

/// Which key signs an id_token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signer {
    Es,
    Es2,
    Rsa,
    /// A key the IdP does not publish, presented under a published kid.
    Foreign,
}

/// What the IdP asserts at the next `/authorize`, and how it builds the id_token.
#[derive(Debug, Clone)]
struct Login {
    subject: String,
    email: Option<String>,
    email_verified: Option<bool>,
    name: String,
    groups: Vec<String>,
    extra: Map<String, Value>,
    alg: &'static str,
    signer: Signer,
    kid: Option<String>,
    issuer: Option<String>,
    audience: Option<Value>,
    expires_in: i64,
    nonce: Option<String>,
}

impl Login {
    fn new(subject: &str, email: &str, groups: &[&str]) -> Self {
        Self {
            subject: subject.to_string(),
            email: Some(email.to_string()),
            email_verified: Some(true),
            name: format!("{subject} Example"),
            groups: groups.iter().map(|g| (*g).to_string()).collect(),
            extra: Map::new(),
            alg: "ES256",
            signer: Signer::Es,
            kid: None,
            issuer: None,
            audience: None,
            expires_in: 300,
            nonce: None,
        }
    }

    fn rs256(mut self) -> Self {
        self.alg = "RS256";
        self.signer = Signer::Rsa;
        self
    }
}

struct Pending {
    redirect_uri: String,
    challenge: String,
    nonce: String,
    login: Login,
}

struct IdpState {
    issuer: String,
    es: EcdsaKeyPair,
    es2: EcdsaKeyPair,
    foreign: EcdsaKeyPair,
    rsa: RsaKeyPair,
    rng: SystemRandom,
    /// kids the JWKS publishes.
    published: Vec<&'static str>,
    next: Option<Login>,
    codes: HashMap<String, Pending>,
    token_calls: usize,
}

type Idp = Arc<Mutex<IdpState>>;

fn es_key(rng: &SystemRandom) -> EcdsaKeyPair {
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, rng).expect("key");
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), rng).expect("pair")
}

fn es_jwk(pair: &EcdsaKeyPair, kid: &str) -> Value {
    let public = pair.public_key().as_ref();
    json!({
        "kty": "EC", "crv": "P-256", "use": "sig", "alg": "ES256", "kid": kid,
        "x": URL_SAFE_NO_PAD.encode(&public[1..33]),
        "y": URL_SAFE_NO_PAD.encode(&public[33..65]),
    })
}

fn b64(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(value.to_string())
}

async fn discovery(State(idp): State<Idp>) -> Json<Value> {
    let issuer = idp.lock().expect("idp").issuer.clone();
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "id_token_signing_alg_values_supported": ["ES256", "RS256"],
    }))
}

async fn jwks(State(idp): State<Idp>) -> Json<Value> {
    let idp = idp.lock().expect("idp");
    let mut keys = Vec::new();
    for kid in &idp.published {
        keys.push(match *kid {
            "es-1" => es_jwk(&idp.es, "es-1"),
            "es-2" => es_jwk(&idp.es2, "es-2"),
            _ => {
                let parts = RsaPublicKeyComponents::<Vec<u8>>::from(idp.rsa.public());
                json!({
                    "kty": "RSA", "use": "sig", "alg": "RS256", "kid": "rs-1",
                    "n": URL_SAFE_NO_PAD.encode(parts.n), "e": URL_SAFE_NO_PAD.encode(parts.e),
                })
            }
        });
    }
    Json(json!({ "keys": keys }))
}

async fn authorize(
    State(idp): State<Idp>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut idp = idp.lock().expect("idp");
    let (Some(login), Some(state), Some(redirect)) = (
        idp.next.take(),
        query.get("state").cloned(),
        query.get("redirect_uri").cloned(),
    ) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if query.get("client_id").map(String::as_str) != Some(CLIENT_ID)
        || query.get("code_challenge_method").map(String::as_str) != Some("S256")
        || query.get("response_type").map(String::as_str) != Some("code")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut bytes = [0u8; 16];
    idp.rng.fill(&mut bytes).expect("rng");
    let code = URL_SAFE_NO_PAD.encode(bytes);
    idp.codes.insert(
        code.clone(),
        Pending {
            redirect_uri: redirect.clone(),
            challenge: query.get("code_challenge").cloned().unwrap_or_default(),
            nonce: query.get("nonce").cloned().unwrap_or_default(),
            login,
        },
    );
    let mut location = reqwest::Url::parse(&redirect).expect("redirect");
    location
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &state);
    (
        StatusCode::FOUND,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

async fn token(
    State(idp): State<Idp>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let mut idp = idp.lock().expect("idp");
    idp.token_calls += 1;
    let refuse =
        |error: &str| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
    // A confidential client: HTTP Basic, as the discovery document prefers.
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|value| STANDARD.decode(value).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok());
    if basic.as_deref() != Some(&format!("{CLIENT_ID}:{CLIENT_SECRET}")) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid_client" })),
        )
            .into_response();
    }
    let Some(pending) = form.get("code").and_then(|code| idp.codes.remove(code)) else {
        return refuse("invalid_grant");
    };
    if form.get("redirect_uri") != Some(&pending.redirect_uri) {
        return refuse("invalid_grant");
    }
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    if URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes())) != pending.challenge {
        return refuse("invalid_grant");
    }
    let login = pending.login;
    let now = chrono::Utc::now().timestamp();
    let mut claims = Map::new();
    claims.insert(
        "iss".into(),
        json!(login.issuer.clone().unwrap_or_else(|| idp.issuer.clone())),
    );
    claims.insert(
        "aud".into(),
        login.audience.clone().unwrap_or(json!(CLIENT_ID)),
    );
    claims.insert("sub".into(), json!(login.subject));
    claims.insert("iat".into(), json!(now));
    claims.insert("exp".into(), json!(now + login.expires_in));
    claims.insert(
        "nonce".into(),
        json!(login.nonce.clone().unwrap_or(pending.nonce)),
    );
    claims.insert("name".into(), json!(login.name));
    claims.insert("groups".into(), json!(login.groups));
    if let Some(email) = &login.email {
        claims.insert("email".into(), json!(email));
    }
    if let Some(verified) = login.email_verified {
        claims.insert("email_verified".into(), json!(verified));
    }
    for (key, value) in &login.extra {
        claims.insert(key.clone(), value.clone());
    }
    let kid = login.kid.clone().unwrap_or_else(|| {
        match login.signer {
            Signer::Es | Signer::Foreign => "es-1",
            Signer::Es2 => "es-2",
            Signer::Rsa => "rs-1",
        }
        .to_string()
    });
    let input = format!(
        "{}.{}",
        b64(&json!({ "alg": login.alg, "kid": kid, "typ": "JWT" })),
        b64(&Value::Object(claims))
    );
    let signature = match login.signer {
        Signer::Es => idp
            .es
            .sign(&idp.rng, input.as_bytes())
            .expect("sign")
            .as_ref()
            .to_vec(),
        Signer::Es2 => idp
            .es2
            .sign(&idp.rng, input.as_bytes())
            .expect("sign")
            .as_ref()
            .to_vec(),
        Signer::Foreign => idp
            .foreign
            .sign(&idp.rng, input.as_bytes())
            .expect("sign")
            .as_ref()
            .to_vec(),
        Signer::Rsa => {
            let mut signature = vec![0u8; idp.rsa.public().modulus_len()];
            idp.rsa
                .sign(
                    &RSA_PKCS1_SHA256,
                    &idp.rng,
                    input.as_bytes(),
                    &mut signature,
                )
                .expect("sign");
            signature
        }
    };
    let signature = if login.alg == "none" {
        String::new()
    } else {
        URL_SAFE_NO_PAD.encode(signature)
    };
    Json(json!({
        "access_token": "in-test-idp-access-token",
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": format!("{input}.{signature}"),
    }))
    .into_response()
}

async fn start_idp() -> Idp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let issuer = format!("http://{}", listener.local_addr().expect("addr"));
    let rng = SystemRandom::new();
    let idp = Arc::new(Mutex::new(IdpState {
        issuer,
        es: es_key(&rng),
        es2: es_key(&rng),
        foreign: es_key(&rng),
        rsa: RsaKeyPair::from_der(RSA_KEY).expect("the fixture is a PKCS#1 RSA key"),
        rng,
        published: vec!["es-1", "rs-1"],
        next: None,
        codes: HashMap::new(),
        token_calls: 0,
    }));
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .with_state(idp.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    idp
}

// ------------------------------------------------------------------ the server under test

/// The two routers: the command surface on the superuser pool, everything else as `quansio_app`.
#[derive(Clone)]
struct Api {
    superuser: Router,
    rls: Router,
}

impl Api {
    fn pick(&self, path: &str) -> Router {
        if path.starts_with("/v1/commands/") {
            self.superuser.clone()
        } else {
            self.rls.clone()
        }
    }
}

struct Harness {
    router: Api,
    admin: sqlx::PgPool,
    name: String,
    idp: Idp,
    issuer: String,
    delivery: Arc<RecordingDelivery>,
    owner: String,
    owner_id: String,
    tenant: String,
    personal_ws: String,
}

impl Harness {
    async fn close(self) {
        common::drop_pool(&self.admin, &self.name).await;
    }
}

fn custody() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("QUANSIO_SECRET_SSO_OPS001", CLIENT_SECRET);
        // Both routers verify each other's access tokens.
        let key =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .expect("signing key");
        std::env::set_var(
            "QUANSIO_AUTH_SIGNING_KEY_PKCS8_B64",
            STANDARD.encode(key.as_ref()),
        );
    });
}

async fn app_role_pool(url: &str) -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
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

async fn harness(prefix: &str, loopback_issuers: bool) -> Option<Harness> {
    custody();
    let name = common::scratch_name(prefix);
    let Some(admin) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return None;
    };
    schema::migrate(&admin).await.expect("migrate");
    let pool = app_role_pool(&common::scratch_url(&name)).await;
    let mut config = EnterpriseConfig::load().expect("config/identity.yaml");
    config.allow_loopback_http_issuers = loopback_issuers;
    let delivery = Arc::new(RecordingDelivery::default());
    let build = |pool: sqlx::PgPool| {
        quansio_server::api::router(
            ApiState::new(pool)
                .expect("state")
                .with_scope_trust(ScopeTrust::SessionOnly)
                .with_magic_link_delivery(delivery.clone())
                .with_enterprise_config(config.clone())
                .expect("enterprise"),
        )
    };
    let router = Api {
        superuser: build(admin.clone()),
        rls: build(pool),
    };
    let idp = start_idp().await;
    let issuer = idp.lock().expect("idp").issuer.clone();
    let owner = sign_in(&router, &delivery, "owner@corp.test").await;
    Some(Harness {
        router,
        admin,
        name,
        idp,
        issuer,
        delivery,
        owner: owner["access_token"].as_str().expect("access").to_string(),
        owner_id: owner["user_id"].as_str().expect("user").to_string(),
        tenant: owner["tenant_id"].as_str().expect("tenant").to_string(),
        personal_ws: owner["workspace_id"]
            .as_str()
            .expect("workspace")
            .to_string(),
    })
}

async fn call(
    router: &Api,
    method: &str,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string())),
        None => builder.body(axum::body::Body::empty()),
    }
    .expect("request");
    let response = router.pick(path).oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn sign_in(router: &Api, delivery: &RecordingDelivery, email: &str) -> Value {
    let (status, _) = call(
        router,
        "POST",
        "/v1/auth/magic-link/start",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let challenge = delivery
        .delivered()
        .into_iter()
        .rev()
        .find(|c| c.email == email)
        .expect("a delivered magic link");
    let (status, body) = call(
        router,
        "POST",
        "/v1/auth/magic-link/complete",
        Some(json!({ "token": challenge.token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn try_sign_in(
    router: &Api,
    delivery: &RecordingDelivery,
    email: &str,
) -> (StatusCode, Value) {
    call(
        router,
        "POST",
        "/v1/auth/magic-link/start",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    let challenge = delivery
        .delivered()
        .into_iter()
        .rev()
        .find(|c| c.email == email)
        .expect("a delivered magic link");
    call(
        router,
        "POST",
        "/v1/auth/magic-link/complete",
        Some(json!({ "token": challenge.token })),
        None,
    )
    .await
}

fn command_id() -> String {
    CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new()).to_string()
}

async fn command(router: &Api, name: &str, params: Value, bearer: &str) -> (StatusCode, Value) {
    command_with_id(router, name, &command_id(), params, bearer).await
}

async fn command_with_id(
    router: &Api,
    name: &str,
    id: &str,
    params: Value,
    bearer: &str,
) -> (StatusCode, Value) {
    call(
        router,
        "POST",
        &format!("/v1/commands/{name}"),
        Some(json!({ "command_id": id, "params": params })),
        Some(bearer),
    )
    .await
}

fn ok(status: StatusCode, body: &Value) {
    assert!(status.is_success(), "{status}: {body}");
}

async fn configure_sso(h: &Harness, extra: Value) -> (StatusCode, Value) {
    let mut params = json!({
        "issuer": h.issuer,
        "client_id": CLIENT_ID,
        "client_secret_handle": SECRET_HANDLE,
        "default_workspace_id": h.personal_ws,
    });
    if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
        params.extend(extra.clone());
    }
    command(&h.router, "ConfigureSso", params, &h.owner).await
}

fn path_with_query(path: &str, pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse(&format!("http://server{path}")).expect("url");
    for (key, value) in pairs {
        url.query_pairs_mut().append_pair(key, value);
    }
    format!("{}?{}", url.path(), url.query().unwrap_or_default())
}

/// The browser half of a sign-in: follow the authorization URL to the IdP and read the redirect.
async fn authorize_at_idp(h: &Harness, login: Login) -> (String, String, String) {
    let (status, start) = call(
        &h.router,
        "GET",
        &path_with_query(
            &format!("/v1/auth/sso/{}/start", h.tenant),
            &[("redirect_uri", REDIRECT)],
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    h.idp.lock().expect("idp").next = Some(login);
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("browser");
    let response = browser
        .get(start["authorization_url"].as_str().expect("url"))
        .send()
        .await
        .expect("authorize");
    assert_eq!(response.status().as_u16(), 302);
    let location = reqwest::Url::parse(
        response.headers()[reqwest::header::LOCATION]
            .to_str()
            .expect("location"),
    )
    .expect("location url");
    let pairs: HashMap<String, String> = location.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], start["state"].as_str().expect("state"));
    (
        pairs["code"].clone(),
        pairs["state"].clone(),
        REDIRECT.to_string(),
    )
}

async fn complete(
    h: &Harness,
    code: &str,
    state: &str,
    redirect: &str,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    call(
        &h.router,
        "POST",
        &format!("/v1/auth/sso/{}/complete", h.tenant),
        Some(json!({
            "code": code, "state": state, "redirect_uri": redirect,
            // What a hostile client might add; the server reads none of it.
            "roles": ["owner"], "tenant_role": "owner", "groups": ["it-admins"],
        })),
        bearer,
    )
    .await
}

async fn sso(h: &Harness, login: Login, bearer: Option<&str>) -> (StatusCode, Value) {
    let (code, state, redirect) = authorize_at_idp(h, login).await;
    complete(h, &code, &state, &redirect, bearer).await
}

async fn tenant_role(h: &Harness, user: &str) -> Option<(String, String)> {
    sqlx::query_as(
        "SELECT role, status FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(&h.tenant)
    .bind(user)
    .fetch_optional(&h.admin)
    .await
    .expect("membership")
}

async fn workspace_roles(h: &Harness, user: &str) -> HashMap<String, String> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT workspace_id, role, status FROM workspace_memberships WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(&h.tenant)
    .bind(user)
    .fetch_all(&h.admin)
    .await
    .expect("workspace memberships");
    rows.into_iter()
        .map(|(workspace, role, status)| (workspace, format!("{role}/{status}")))
        .collect()
}

async fn live_sessions(h: &Harness, user: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM sessions WHERE tenant_id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(&h.tenant)
    .bind(user)
    .fetch_one(&h.admin)
    .await
    .expect("sessions")
}

async fn create_workspace(h: &Harness, name: &str) -> String {
    let (status, body) = command(
        &h.router,
        "CreateWorkspace",
        json!({ "name": name }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    body["result"]["workspace_id"]
        .as_str()
        .expect("workspace")
        .to_string()
}

async fn principal(h: &Harness, name: &str, scopes: &[&str]) -> (String, String) {
    let (status, body) = command(
        &h.router,
        "CreateServicePrincipal",
        json!({ "name": name, "scopes": scopes }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    (
        body["result"]["service_principal"]["id"]
            .as_str()
            .expect("id")
            .to_string(),
        body["result"]["secret"].as_str().expect("key").to_string(),
    )
}

// ------------------------------------------------------------------ SSO

#[tokio::test]
async fn sso_sign_in_maps_groups_server_side_and_ignores_client_claims() {
    let Some(h) = harness("ops001_sso", true).await else {
        return;
    };
    let eng = create_workspace(&h, "Engineering").await;
    let (status, body) = configure_sso(&h, json!({})).await;
    ok(status, &body);
    assert_eq!(body["result"]["sso"]["client_secret_handle"], SECRET_HANDLE);
    for (group, workspace, role) in [
        ("eng", Some(eng.as_str()), "editor"),
        ("eng-leads", Some(eng.as_str()), "approver"),
        ("it-admins", None, "admin"),
        ("finance", None, "billing"),
    ] {
        let mut params = json!({ "external_group": group, "role": role });
        if let Some(workspace) = workspace {
            params["workspace_id"] = json!(workspace);
        }
        let (status, body) = command(&h.router, "SetGroupRoleMapping", params, &h.owner).await;
        ok(status, &body);
    }
    // A mapping never grants owner.
    let (status, _) = command(
        &h.router,
        "SetGroupRoleMapping",
        json!({ "external_group": "root", "role": "owner" }),
        &h.owner,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Ada is in `eng`; the id_token also carries role claims and the request body names owner.
    let mut login = Login::new("ada-sub", "ada@corp.test", &["eng"]);
    login
        .extra
        .insert("roles".into(), json!(["owner", "admin"]));
    login.extra.insert("role".into(), json!("admin"));
    login.extra.insert("tenant_role".into(), json!("owner"));
    let (status, ada) = sso(&h, login.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{ada}");
    assert_eq!(ada["tenant_id"], h.tenant);
    assert_eq!(ada["onboarded"], true);
    assert_eq!(ada["workspace_id"], eng, "lands in the mapped workspace");
    let ada_id = ada["user_id"].as_str().expect("user").to_string();
    assert_eq!(
        tenant_role(&h, &ada_id).await,
        Some(("member".to_string(), "active".to_string())),
        "no tenant group matched: member, whatever the token and the client say"
    );
    let roles = workspace_roles(&h, &ada_id).await;
    assert_eq!(roles.get(&eng).map(String::as_str), Some("editor/active"));
    assert!(
        !roles.contains_key(&h.personal_ws),
        "a mapped workspace, so no default"
    );
    let ada_token = ada["access_token"].as_str().expect("access").to_string();
    let (status, _) = call(&h.router, "GET", "/v1/sso", None, Some(&ada_token)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a member cannot read the SSO settings"
    );
    let session_method: String =
        sqlx::query_scalar("SELECT auth_method FROM sessions WHERE id = $1")
            .bind(ada["session_id"].as_str().expect("session"))
            .fetch_one(&h.admin)
            .await
            .expect("session");
    assert_eq!(session_method, "sso");

    // New groups at the next sign-in: the highest mapped role wins in each scope.
    let mut promoted = Login::new(
        "ada-sub",
        "ada@corp.test",
        &["eng", "eng-leads", "finance", "it-admins"],
    );
    promoted.name = "Ada Lovelace".into();
    let (status, again) = sso(&h, promoted, None).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["user_id"],
        ada_id.as_str(),
        "the linked subject is the same account"
    );
    assert_eq!(again["onboarded"], false);
    assert_eq!(
        tenant_role(&h, &ada_id).await.map(|r| r.0).as_deref(),
        Some("admin")
    );
    assert_eq!(workspace_roles(&h, &ada_id).await[&eng], "approver/active");
    let (status, _) = call(
        &h.router,
        "GET",
        "/v1/sso",
        None,
        again["access_token"].as_str(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Groups removed at the IdP drop the roles; removing a mapping recomputes at once.
    let (status, _) = sso(&h, Login::new("ada-sub", "ada@corp.test", &["eng"]), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        tenant_role(&h, &ada_id).await.map(|r| r.0).as_deref(),
        Some("member")
    );
    let (status, body) = command(
        &h.router,
        "RemoveGroupRoleMapping",
        json!({ "external_group": "eng", "workspace_id": eng }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    assert_eq!(body["result"]["members_changed"], 1);
    assert_eq!(
        workspace_roles(&h, &ada_id).await[&eng],
        "editor/removed",
        "the workspace is still managed (eng-leads) and no group of Ada's maps there now"
    );

    // RS256 signs too, and a user no mapping places lands in the default workspace as viewer.
    let (status, bob) = sso(
        &h,
        Login::new("bob-sub", "bob@corp.test", &["unmapped"]).rs256(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bob}");
    let bob_id = bob["user_id"].as_str().expect("user").to_string();
    assert_eq!(bob["workspace_id"], h.personal_ws);
    assert_eq!(
        workspace_roles(&h, &bob_id).await[&h.personal_ws],
        "viewer/active"
    );

    // The events and the audit trail name what happened.
    let provisioned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runtime_events WHERE tenant_id = $1 AND type = 'member.provisioned'",
    )
    .bind(&h.tenant)
    .fetch_one(&h.admin)
    .await
    .expect("events");
    assert_eq!(provisioned, 2);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_entries WHERE tenant_id = $1 AND action = 'sso.sign_in'",
    )
    .bind(&h.tenant)
    .fetch_one(&h.admin)
    .await
    .expect("audit");
    assert_eq!(audited, 4);
    h.close().await;
}

#[tokio::test]
async fn id_tokens_that_do_not_verify_are_refused_and_issue_no_session() {
    let Some(h) = harness("ops001_idtoken", true).await else {
        return;
    };
    let (status, body) = configure_sso(&h, json!({})).await;
    ok(status, &body);
    let base = || Login::new("eve-sub", "eve@corp.test", &[]);
    let mut cases: Vec<(&str, Login)> = Vec::new();
    let mut login = base();
    login.issuer = Some("https://idp.evil.test".into());
    cases.push(("another issuer", login));
    let mut login = base();
    login.audience = Some(json!("someone-else"));
    cases.push(("another audience", login));
    let mut login = base();
    login.audience = Some(json!([CLIENT_ID, "someone-else"]));
    cases.push(("several audiences and no azp", login));
    let mut login = base();
    login.expires_in = -600;
    cases.push(("expired", login));
    let mut login = base();
    login.signer = Signer::Foreign;
    cases.push(("signed by a key the IdP does not publish", login));
    let mut login = base();
    login.alg = "none";
    cases.push(("alg none", login));
    let mut login = base();
    login.alg = "HS256";
    cases.push(("HMAC", login));
    let mut login = base();
    login.kid = Some("never-published".into());
    cases.push(("a kid that no JWKS fetch holds", login));
    let mut login = base();
    login.nonce = Some("another-requests-nonce".into());
    cases.push(("another request's nonce", login));
    let mut login = base();
    login.email_verified = Some(false);
    cases.push(("an unverified email", login));
    let mut login = base();
    login.email = None;
    cases.push(("no email", login));
    for (why, login) in cases {
        let (status, body) = sso(&h, login, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{why}: {body}");
        assert_eq!(body["code"], "AUTH_INVALID_TOKEN", "{why}: {body}");
    }
    let accounts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE primary_email = 'eve@corp.test'")
            .fetch_one(&h.admin)
            .await
            .expect("users");
    assert_eq!(accounts, 0, "no refused token provisions anything");
    let sessions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sessions WHERE auth_method = 'sso'")
            .fetch_one(&h.admin)
            .await
            .expect("sessions");
    assert_eq!(sessions, 0, "no refused token issues a session");

    // The state is single-use, bound to its redirect, and consumed before the code is exchanged.
    let (code, state, redirect) = authorize_at_idp(&h, base()).await;
    let (status, body) = complete(&h, &code, &state, "http://127.0.0.1:9/elsewhere", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, _) = complete(&h, &code, &state, &redirect, None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the state was consumed by the refused attempt"
    );
    let (code, state, redirect) = authorize_at_idp(&h, base()).await;
    let (status, body) = complete(&h, &code, &state, &redirect, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = h.idp.lock().expect("idp").token_calls;
    let (status, _) = complete(&h, &code, &state, &redirect, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a replayed state");
    assert_eq!(
        h.idp.lock().expect("idp").token_calls,
        calls,
        "a replay never reaches the IdP"
    );
    let (status, _) = complete(&h, "forged-code", "forged-state", &redirect, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Key rotation: the IdP publishes a new key; a token signed with it verifies after one refetch.
    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
    {
        let mut idp = h.idp.lock().expect("idp");
        idp.published = vec!["es-2", "rs-1"];
    }
    let mut rotated = base();
    rotated.signer = Signer::Es2;
    let (status, body) = sso(&h, rotated, None).await;
    assert_eq!(status, StatusCode::OK, "rotation: {body}");

    // An http issuer is refused unless the deployment admits loopback issuers.
    h.close().await;
    let Some(strict) = harness("ops001_https", false).await else {
        return;
    };
    let (status, body) = configure_sso(&strict, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("https"),
        "{body}"
    );
    let (status, body) = configure_sso(
        &strict,
        json!({ "issuer": "https://127.0.0.1:1/", "client_secret_handle": "sk-live-raw-secret" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a raw secret is not a custody handle: {body}"
    );
    strict.close().await;
}

// ------------------------------------------------------------------ SCIM

#[tokio::test]
async fn scim_deprovision_revokes_access_promptly() {
    let Some(h) = harness("ops001_scim", true).await else {
        return;
    };
    let eng = create_workspace(&h, "Engineering").await;
    let (status, body) = configure_sso(&h, json!({})).await;
    ok(status, &body);
    let (status, body) = command(
        &h.router,
        "SetGroupRoleMapping",
        json!({ "external_group": "eng", "workspace_id": eng, "role": "editor" }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let owner_eng = workspace_roles(&h, &h.owner_id).await.get(&eng).cloned();
    assert!(
        owner_eng
            .as_deref()
            .is_some_and(|role| role.ends_with("/active")),
        "{owner_eng:?}"
    );
    let (_, idp_key) = principal(&h, "okta-provisioning", &["Provisioning"]).await;
    let (_, other_key) = principal(&h, "ci", &["Work"]).await;

    // Only a principal holding Provisioning reaches SCIM.
    let users = "/scim/v2/Users";
    let (status, body) = call(&h.router, "GET", users, None, None).await;
    assert_eq!(
        (status, body["schemas"][0].as_str()),
        (
            StatusCode::UNAUTHORIZED,
            Some("urn:ietf:params:scim:api:messages:2.0:Error")
        )
    );
    let (status, _) = call(&h.router, "GET", users, None, Some(&h.owner)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a person's session is not a provisioning credential"
    );
    let (status, _) = call(&h.router, "GET", users, None, Some(&other_key)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a principal without Provisioning"
    );
    let (status, config) = call(
        &h.router,
        "GET",
        "/scim/v2/ServiceProviderConfig",
        None,
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config["patch"]["supported"], true);

    // Provision Carol and her group.
    let (status, carol) = call(
        &h.router,
        "POST",
        users,
        Some(json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
            "userName": "Carol@Corp.test", "externalId": "okta-carol",
            "name": { "givenName": "Carol", "familyName": "Shaw" }, "active": true,
            "title": "Engineer", "roles": [{ "value": "owner" }],
        })),
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{carol}");
    let carol_id = carol["id"].as_str().expect("id").to_string();
    assert!(carol_id.starts_with("usr_"));
    assert_eq!(carol["userName"], "carol@corp.test");
    assert_eq!(carol["active"], true);
    let (status, dup) = call(
        &h.router,
        "POST",
        users,
        Some(json!({ "userName": "carol@corp.test" })),
        Some(&idp_key),
    )
    .await;
    assert_eq!(
        (status, dup["scimType"].as_str()),
        (StatusCode::CONFLICT, Some("uniqueness"))
    );
    let (status, group) = call(
        &h.router,
        "POST",
        "/scim/v2/Groups",
        Some(json!({ "displayName": "eng", "members": [{ "value": carol_id }] })),
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{group}");
    assert!(group["id"].as_str().expect("group").starts_with("dgr_"));
    assert_eq!(
        workspace_roles(&h, &h.owner_id).await.get(&eng),
        owner_eng.as_ref(),
        "an owner's memberships are never changed by a mapping"
    );
    assert_eq!(
        tenant_role(&h, &carol_id).await.map(|r| r.0).as_deref(),
        Some("member"),
        "SCIM roles are ignored"
    );
    assert_eq!(workspace_roles(&h, &carol_id).await[&eng], "editor/active");
    let (status, found) = call(
        &h.router,
        "GET",
        &path_with_query(users, &[("filter", "userName eq \"carol@corp.test\"")]),
        None,
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["totalResults"], 1);
    assert_eq!(found["Resources"][0]["groups"][0]["display"], "eng");
    let (status, _) = call(
        &h.router,
        "GET",
        &path_with_query(users, &[("filter", "title co \"x\"")]),
        None,
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Carol signs in through the IdP: her provisioned account links on first sign-in.
    let (status, session) = sso(&h, Login::new("carol-sub", "carol@corp.test", &[]), None).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(session["user_id"], carol_id.as_str());
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    let refresh = session["refresh_token"]
        .as_str()
        .expect("refresh")
        .to_string();
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&access)).await;
    assert_eq!(status, StatusCode::OK);

    // Entra-style deprovision: the very next request with any of her tokens is refused.
    let (status, patched) = call(
        &h.router,
        "PATCH",
        &format!("{users}/{carol_id}"),
        Some(json!({
            "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
            "Operations": [{ "op": "Replace", "path": "active", "value": "False" }],
        })),
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["active"], false);
    let (status, body) = call(&h.router, "GET", "/v1/me", None, Some(&access)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, _) = call(
        &h.router,
        "POST",
        "/v1/auth/refresh",
        Some(json!({ "refresh_token": refresh })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the refresh token died with the session"
    );
    let (status, body) = sso(&h, Login::new("carol-sub", "carol@corp.test", &[]), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a suspended membership cannot sign in: {body}"
    );
    assert_eq!(live_sessions(&h, &carol_id).await, 0);
    assert_eq!(
        tenant_role(&h, &carol_id).await.map(|r| r.1).as_deref(),
        Some("suspended")
    );
    assert_eq!(
        workspace_roles(&h, &carol_id).await[&eng],
        "editor/suspended"
    );
    let (event_actor,): (Value,) = sqlx::query_as(
        "SELECT actor FROM runtime_events WHERE tenant_id = $1 AND type = 'member.updated' \
         AND payload->>'user_id' = $2 ORDER BY sequence DESC LIMIT 1",
    )
    .bind(&h.tenant)
    .bind(&carol_id)
    .fetch_one(&h.admin)
    .await
    .expect("event");
    assert_eq!(event_actor["kind"], "service");

    // Restored by the directory, she signs in again with her mapped role.
    let (status, _) = call(
        &h.router,
        "PATCH",
        &format!("{users}/{carol_id}"),
        Some(json!({ "Operations": [{ "op": "replace", "value": { "active": true } }] })),
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, session) = sso(&h, Login::new("carol-sub", "carol@corp.test", &[]), None).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let access = session["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    assert_eq!(workspace_roles(&h, &carol_id).await[&eng], "editor/active");

    // Leaving the group drops the role; DELETE ends the membership and every session.
    let group_id = group["id"].as_str().expect("group");
    let (status, body) = call(
        &h.router,
        "PATCH",
        &format!("/scim/v2/Groups/{group_id}"),
        Some(json!({ "Operations": [{ "op": "remove", "path": format!("members[value eq \"{carol_id}\"]") }] })),
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(workspace_roles(&h, &carol_id).await[&eng], "editor/removed");
    let (status, _) = call(
        &h.router,
        "DELETE",
        &format!("{users}/{carol_id}"),
        None,
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&access)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &h.router,
        "GET",
        &format!("{users}/{carol_id}"),
        None,
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The directory cannot remove the tenant's last owner.
    let (status, body) = call(
        &h.router,
        "PATCH",
        &format!("{users}/{}", h.owner_id),
        Some(json!({ "Operations": [{ "op": "replace", "path": "active", "value": false }] })),
        Some(&idp_key),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&h.owner)).await;
    assert_eq!(status, StatusCode::OK);

    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_entries WHERE tenant_id = $1 AND actor->>'kind' = 'service' ORDER BY occurred_at",
    )
    .bind(&h.tenant)
    .fetch_all(&h.admin)
    .await
    .expect("audit");
    for action in [
        "scim.create_user",
        "scim.create_group",
        "scim.patch_user",
        "scim.patch_group",
        "scim.delete_user",
    ] {
        assert!(
            audited.iter().any(|a| a == action),
            "{action} is audited: {audited:?}"
        );
    }
    h.close().await;
}

// ------------------------------------------------------------------ sessions

#[tokio::test]
async fn token_and_session_revocation_take_effect_on_the_next_request() {
    let Some(h) = harness("ops001_revoke", true).await else {
        return;
    };
    let (status, body) = command(
        &h.router,
        "InviteMember",
        json!({ "email": "dave@corp.test", "role": "member", "workspace_id": h.personal_ws, "workspace_role": "editor" }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let desktop = sign_in(&h.router, &h.delivery, "dave@corp.test").await;
    assert_eq!(desktop["tenant_id"], h.tenant);
    let dave_id = desktop["user_id"].as_str().expect("user").to_string();
    let first = desktop["access_token"]
        .as_str()
        .expect("access")
        .to_string();
    let second = sign_in(&h.router, &h.delivery, "dave@corp.test").await;
    let current = second["access_token"].as_str().expect("access").to_string();

    // Dave sees his two sessions and ends the other one.
    let (status, listed) = call(&h.router, "GET", "/v1/auth/sessions", None, Some(&current)).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let sessions = listed["sessions"].as_array().expect("sessions");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    let other = sessions
        .iter()
        .find(|s| s["current"] == false)
        .expect("other")["session_id"]
        .as_str()
        .expect("id")
        .to_string();
    let (status, _) = call(
        &h.router,
        "POST",
        &format!("/v1/auth/sessions/{other}/revoke"),
        Some(json!({})),
        Some(&current),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&first)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the revoked session");
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&current)).await;
    assert_eq!(status, StatusCode::OK, "the current session");
    let owner_session = call(&h.router, "GET", "/v1/auth/sessions", None, Some(&h.owner))
        .await
        .1["sessions"][0]["session_id"]
        .clone();
    let (status, _) = call(
        &h.router,
        "POST",
        &format!(
            "/v1/auth/sessions/{}/revoke",
            owner_session.as_str().expect("owner session")
        ),
        Some(json!({})),
        Some(&current),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nobody else's session");

    // A member cannot revoke others; an administrator can, at once.
    let (status, _) = command(
        &h.router,
        "RevokeUserSessions",
        json!({ "user_id": h.owner_id }),
        &current,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = command(
        &h.router,
        "RevokeUserSessions",
        json!({ "user_id": dave_id }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    assert_eq!(body["result"]["revoked_sessions"], 1);
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&current)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Suspending or removing the membership revokes too, and a refresh cannot revive it.
    let again = sign_in(&h.router, &h.delivery, "dave@corp.test").await;
    let token = again["access_token"].as_str().expect("access").to_string();
    let membership: String = sqlx::query_scalar(
        "SELECT id FROM tenant_memberships WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(&h.tenant)
    .bind(&dave_id)
    .fetch_one(&h.admin)
    .await
    .expect("membership");
    let (status, body) = command(
        &h.router,
        "UpdateMembership",
        json!({ "membership_id": membership, "status": "suspended" }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &h.router,
        "POST",
        "/v1/auth/refresh",
        Some(json!({ "refresh_token": again["refresh_token"] })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = command(
        &h.router,
        "UpdateMembership",
        json!({ "membership_id": membership, "status": "active" }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let last = sign_in(&h.router, &h.delivery, "dave@corp.test").await;
    let (status, body) = command(
        &h.router,
        "RemoveMember",
        json!({ "membership_id": membership }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let (status, _) = call(
        &h.router,
        "GET",
        "/v1/me",
        None,
        last["access_token"].as_str(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(live_sessions(&h, &dave_id).await, 0);

    // Defense in depth: whatever path suspends a membership or an account, its unrevoked sessions
    // stop working on the next request, because authentication reads both every time.
    let (status, body) = command(
        &h.router,
        "InviteMember",
        json!({ "email": "hank@corp.test", "role": "member", "workspace_id": h.personal_ws }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let hank = sign_in(&h.router, &h.delivery, "hank@corp.test").await;
    let hank_id = hank["user_id"].as_str().expect("user").to_string();
    let hank_token = hank["access_token"].as_str().expect("access").to_string();
    for (statement, what) in [
        (
            "UPDATE tenant_memberships SET status = $3 WHERE tenant_id = $1 AND user_id = $2",
            "membership",
        ),
        (
            "UPDATE users SET status = $3 WHERE id = $2 AND $1 <> ''",
            "account",
        ),
    ] {
        sqlx::query(statement)
            .bind(&h.tenant)
            .bind(&hank_id)
            .bind("suspended")
            .execute(&h.admin)
            .await
            .expect("suspend");
        let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&hank_token)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a suspended {what} refuses its live session"
        );
        sqlx::query(statement)
            .bind(&h.tenant)
            .bind(&hank_id)
            .bind("active")
            .execute(&h.admin)
            .await
            .expect("restore");
        let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&hank_token)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the session itself was never revoked"
        );
    }
    h.close().await;
}

// ------------------------------------------------------------------ service principals

#[tokio::test]
async fn service_principal_scopes_are_explicit_and_auditable() {
    let Some(h) = harness("ops001_principal", true).await else {
        return;
    };
    for (scopes, why) in [
        (json!([]), "no scope"),
        (json!(["Everything"]), "not a command family"),
        (json!(["Trust"]), "approvals stay human"),
        (json!(["Work", "Work"]), "a repeated scope"),
    ] {
        let (status, body) = command(
            &h.router,
            "CreateServicePrincipal",
            json!({ "name": "x", "scopes": scopes }),
            &h.owner,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }

    // The key is returned once and stored only as its argon2id hash.
    let id = command_id();
    let (status, created) = command_with_id(
        &h.router,
        "CreateServicePrincipal",
        &id,
        json!({ "name": "ci-bot", "scopes": ["Conversation"] }),
        &h.owner,
    )
    .await;
    ok(status, &created);
    let principal_id = created["result"]["service_principal"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let key = created["result"]["secret"]
        .as_str()
        .expect("key")
        .to_string();
    assert!(
        key.starts_with(&format!("qsk_{}_", &principal_id[3..])),
        "{key}"
    );
    let (hash, scopes): (String, Value) =
        sqlx::query_as("SELECT key_hash, scopes FROM service_principals WHERE id = $1")
            .bind(&principal_id)
            .fetch_one(&h.admin)
            .await
            .expect("row");
    assert!(hash.starts_with("$argon2id$"), "{hash}");
    assert!(
        !hash.contains(&key[31..]),
        "the stored hash holds no key material"
    );
    assert_eq!(scopes, json!(["Conversation"]));
    let (status, replayed) = command_with_id(
        &h.router,
        "CreateServicePrincipal",
        &id,
        json!({ "name": "ci-bot", "scopes": ["Conversation"] }),
        &h.owner,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(
        replayed["result"]["secret"],
        Value::Null,
        "a replay never returns the key again"
    );
    let logged: String =
        sqlx::query_scalar("SELECT result::text FROM commands WHERE command_id = $1")
            .bind(&id)
            .fetch_one(&h.admin)
            .await
            .expect("command log");
    assert!(
        !logged.contains(&key),
        "the command log never holds the key"
    );
    let (status, listed) = call(
        &h.router,
        "GET",
        "/v1/service-principals",
        None,
        Some(&h.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!listed.to_string().contains("argon2") && !listed.to_string().contains(&key));

    // In scope: a Conversation command runs as the principal.
    let (status, thread) = command(
        &h.router,
        "CreateThread",
        json!({ "workspace_id": h.personal_ws, "kind": "objective", "title": "Nightly build" }),
        &key,
    )
    .await;
    assert!(status.is_success(), "{status}: {thread}");
    // Out of scope: refused before it reaches an owner, and audited.
    let (status, refused) = command(
        &h.router,
        "PauseRun",
        json!({ "run_id": "run_01J8Z3K6F1N8VQ2X5W9Y0PS001" }),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "SCOPE_FORBIDDEN");
    // Never: principal management, SSO, reads, the session endpoints.
    let (_, identity_key) = principal(&h, "identity-bot", &["Identity"]).await;
    let (status, _) = command(
        &h.router,
        "CreateServicePrincipal",
        json!({ "name": "y", "scopes": ["Work"] }),
        &identity_key,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a principal cannot mint principals"
    );
    let (status, _) = command(
        &h.router,
        "ConfigureSso",
        json!({ "issuer": h.issuer, "client_id": "x" }),
        &identity_key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "nor configure SSO");
    let threads = format!("/v1/threads?workspace_id={}", h.personal_ws);
    for path in [
        "/v1/me",
        threads.as_str(),
        "/v1/audit",
        "/v1/service-principals",
        "/v1/sso",
        "/v1/auth/sessions",
    ] {
        let (status, _) = call(&h.router, "GET", path, None, Some(&key)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{path} refuses a principal key"
        );
    }

    // Every principal command is in the audit log, applied or refused, as a service actor.
    let rows: Vec<(Value, String, String)> = sqlx::query_as(
        "SELECT actor, action, decision FROM audit_entries WHERE tenant_id = $1 \
         AND actor->>'kind' = 'service' ORDER BY occurred_at",
    )
    .bind(&h.tenant)
    .fetch_all(&h.admin)
    .await
    .expect("audit");
    let named = |action: &str, decision: &str| {
        rows.iter().any(|(actor, a, d)| {
            actor["id"] == principal_id.as_str() && a == action && d == decision
        })
    };
    assert!(named("CreateThread", "applied"), "{rows:?}");
    assert!(named("PauseRun", "refused"), "{rows:?}");
    let (status, log) = call(&h.router, "GET", "/v1/audit", None, Some(&h.owner)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(log.to_string().contains(&principal_id));
    let used: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_used_at FROM service_principals WHERE id = $1")
            .bind(&principal_id)
            .fetch_one(&h.admin)
            .await
            .expect("used");
    assert!(used.is_some());

    // A wrong secret under a real id, and a revoked key, are refused on the next request.
    let mut forged = key.clone();
    forged.replace_range(forged.len() - 4.., "AAAA");
    let (status, _) = command(
        &h.router,
        "CreateThread",
        json!({ "workspace_id": h.personal_ws, "kind": "objective" }),
        &forged,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = command(
        &h.router,
        "RevokeServicePrincipal",
        json!({ "service_principal_id": principal_id }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let (status, _) = command(
        &h.router,
        "CreateThread",
        json!({ "workspace_id": h.personal_ws, "kind": "objective" }),
        &key,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "revocation takes effect on the next request"
    );
    h.close().await;
}

// ------------------------------------------------------------------ linking and enforcement

#[tokio::test]
async fn existing_accounts_link_only_with_consent_and_enforcement_admits_only_sso() {
    let Some(h) = harness("ops001_link", true).await else {
        return;
    };
    let (status, body) = configure_sso(&h, json!({})).await;
    ok(status, &body);

    // Erin already has her own account (and personal tenant); the organization adds her.
    let erin = sign_in(&h.router, &h.delivery, "erin@corp.test").await;
    let erin_id = erin["user_id"].as_str().expect("user").to_string();
    assert_ne!(erin["tenant_id"], h.tenant);
    let (status, body) = command(
        &h.router,
        "InviteMember",
        json!({ "email": "erin@corp.test", "role": "member", "workspace_id": h.personal_ws }),
        &h.owner,
    )
    .await;
    ok(status, &body);

    // The IdP alone cannot take over an account it did not create.
    let (status, body) = sso(&h, Login::new("erin-sub", "erin@corp.test", &[]), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "CONFLICT_STATE");
    // Erin, signed in to her own account, completes SSO: that is her consent to link.
    let (status, linked) = sso(
        &h,
        Login::new("erin-sub", "erin@corp.test", &[]),
        erin["access_token"].as_str(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["user_id"], erin_id.as_str());
    assert_eq!(linked["tenant_id"], h.tenant);
    let (status, again) = sso(&h, Login::new("erin-sub", "erin@corp.test", &[]), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the linked subject signs in: {again}"
    );
    let (status, body) = sso(
        &h,
        Login::new("erin-other-sub", "erin@corp.test", &[]),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "another subject for the same address: {body}"
    );
    // Consent is the account's own: someone else's session links nothing.
    let (status, body) = sso(
        &h,
        Login::new("mallory-sub", "erin@corp.test", &[]),
        Some(&h.owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Frank's only tenant is the organization; before enforcement a magic link lands there.
    let (status, body) = command(
        &h.router,
        "InviteMember",
        json!({ "email": "frank@corp.test", "role": "member", "workspace_id": h.personal_ws }),
        &h.owner,
    )
    .await;
    ok(status, &body);
    let frank = sign_in(&h.router, &h.delivery, "frank@corp.test").await;
    assert_eq!(frank["tenant_id"], h.tenant);
    let frank_token = frank["access_token"].as_str().expect("access").to_string();

    // Enforcement revokes non-SSO sessions of non-owners and refuses new ones; owners keep a
    // break-glass sign-in; SSO still works.
    let (status, body) = configure_sso(&h, json!({ "enforced": true })).await;
    ok(status, &body);
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&frank_token)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the pre-enforcement session is gone"
    );
    let (status, body) = try_sign_in(&h.router, &h.delivery, "frank@corp.test").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("single sign-on"),
        "{body}"
    );
    let (status, _) = call(&h.router, "GET", "/v1/me", None, Some(&h.owner)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the owner's session survives enforcement"
    );
    let owner_again = sign_in(&h.router, &h.delivery, "owner@corp.test").await;
    assert_eq!(owner_again["tenant_id"], h.tenant, "break-glass");
    let (status, body) = sso(&h, Login::new("frank-sub", "frank@corp.test", &[]), None).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "Frank's account came from an invitation, not the IdP: {body}"
    );
    let (status, body) = sso(&h, Login::new("gina-sub", "gina@corp.test", &[]), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a new address is provisioned through SSO: {body}"
    );
    // Erin's own sign-in still lands in her personal tenant, which does not enforce SSO.
    let (status, body) = try_sign_in(&h.router, &h.delivery, "erin@corp.test").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["tenant_id"], h.tenant);
    h.close().await;
}

// ------------------------------------------------------------------ a real IdP

#[tokio::test]
async fn a_real_idp_issues_id_tokens_the_verifier_accepts() {
    let values: Vec<Option<String>> = LIVE_OIDC_ENV
        .iter()
        .map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .collect();
    let [Some(issuer), Some(client_id), Some(secret), Some(username), Some(password)] =
        values.as_slice()
    else {
        println!(
            "BLOCKED_EXTERNAL: {} are not all set; the real-IdP proof did not run",
            LIVE_OIDC_ENV.join(", ")
        );
        return;
    };
    let http = reqwest::Client::new();
    let discovery: Value = http
        .get(format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ))
        .send()
        .await
        .expect("discovery")
        .json()
        .await
        .expect("discovery json");
    assert_eq!(
        discovery["issuer"],
        issuer.as_str(),
        "the realm names itself as the configured issuer"
    );
    let jwks: Jwks = http
        .get(discovery["jwks_uri"].as_str().expect("jwks_uri"))
        .send()
        .await
        .expect("jwks")
        .json()
        .await
        .expect("jwks json");
    let token: Value = http
        .post(
            discovery["token_endpoint"]
                .as_str()
                .expect("token_endpoint"),
        )
        .basic_auth(client_id, Some(secret))
        .form(&[
            ("grant_type", "password"),
            ("username", username.as_str()),
            ("password", password.as_str()),
            ("scope", "openid email profile"),
        ])
        .send()
        .await
        .expect("token")
        .json()
        .await
        .expect("token json");
    let id_token = token["id_token"]
        .as_str()
        .expect("the sandbox returns an id_token");
    let verified = jwt::verify(
        id_token,
        &jwks,
        &Expectations {
            issuer,
            client_id,
            nonce_sha256: None,
            now: chrono::Utc::now().timestamp(),
            skew: 60,
        },
    )
    .expect("a real id_token verifies against the realm's published keys");
    assert!(!verified.subject.is_empty());
    let mut tampered: Vec<String> = id_token.split('.').map(str::to_string).collect();
    tampered[1] = b64(
        &json!({ "iss": issuer, "aud": client_id, "sub": "someone-else", "iat": 0, "exp": i64::MAX }),
    );
    assert!(jwt::verify(
        &tampered.join("."),
        &jwks,
        &Expectations {
            issuer,
            client_id,
            nonce_sha256: None,
            now: chrono::Utc::now().timestamp(),
            skew: 60,
        }
    )
    .is_err());
    println!("REAL_BOUNDARY: {issuer} discovery, JWKS and a password-grant id_token verified");
}
