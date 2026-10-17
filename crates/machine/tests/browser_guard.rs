//! The hardened browser's durable state, user-only decisions and secret placeholders, against real
//! PostgreSQL (ADP-001, DOMAIN.md §8.4.1 C, E, F).
//!
//! The worker half of all of this — the driver against a real Chrome — is in `crates/qworkerd/tests`.
//! This suite proves the half that has to hold on the control plane:
//!
//! * the worker's report of itself is parsed strictly and bounded, stored on the session row, and a
//!   report that raced a user's decision cannot undo it;
//! * a certificate error or a media permission can be decided only by a user: an agent actor is refused
//!   and the row is left byte-for-byte as it was; a decision leaves the user's identity behind and
//!   returns the receipt the worker acts on;
//! * a `{{secret:<name>.<field>}}` resolves through the secret broker's own handles, scoped to the typing
//!   tool, with the allowed origins sealed inside the material; and the value is in no stored column
//!   but the sealed ciphertext.
//!
//! When the environment provides no database the suite reports `BLOCKED_EXTERNAL` and returns, so a
//! missing dev stack is never a pass.

mod common;

use quansio_core::OsEntropy;
use quansio_machine::control::browser::{
    BrowserSecretError, BrowserSecretMaterial, BrowserSecretRequest, BrowserSecrets,
    BrowserSessionError, BrowserSessionStore, Decision, DecisionActor, GuardReport,
    BROWSER_SECRET_TOOL,
};
use quansio_machine::control::{MachineControl, NewTarget, Substrate, TargetClass};
use quansio_machine::secrets::{LocalMasterKeyProvider, NewSecret, SecretBroker};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0GGGGG";
const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const TAB: &str = "CDP-TAB-1";
const AT: &str = "2026-10-03T10:00:00Z";
const LATER: &str = "2026-10-03T10:00:30Z";
const EXPIRES: &str = "2026-10-03T10:01:00Z";
const EFFECT: &str = "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
const CANARY: &str = "hunter2-CANARY-7731";

fn provider() -> LocalMasterKeyProvider {
    LocalMasterKeyProvider::from_master_key(&[42u8; 32], "local:kek-v1").expect("a master key")
}

async fn seed(pool: &PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    common::seed_tenant(pool, TENANT, USER, WORKSPACE).await;
    common::seed_tenant(pool, OTHER_TENANT, OTHER_USER, OTHER_WORKSPACE).await;
    let mut pooled = pool.acquire().await.expect("acquire");
    let conn: &mut PgConnection = &mut pooled;
    MachineControl::register(
        conn,
        TENANT,
        &NewTarget {
            id: TARGET.to_string(),
            workspace_id: WORKSPACE.to_string(),
            class: TargetClass::IsolatedTaskRuntime,
            substrate: Substrate::CloudMicrovm,
            image_digest: None,
            desired_state: None,
        },
    )
    .await
    .expect("register target");
    BrowserSessionStore::open(conn, TENANT, SESSION, TARGET, None, None)
        .await
        .expect("open session");
    BrowserSessionStore::record_page(conn, TENANT, SESSION, TAB, "https://example.com/", "Home")
        .await
        .expect("record page");
    pooled
}

fn report(certificate_status: &str) -> Value {
    json!({
        "tabs": [{
            "tab_id": TAB,
            "url": "https://example.com/",
            "document_revision": "rev_0123456789abcdef",
            "dialog": { "id": "dlg_1", "kind": "confirm", "message": "Sure?", "url": "https://example.com/" },
            "certificates": [{
                "id": "crt_1", "origin": "https://example.com", "error_type": "ERR_CERT_AUTHORITY_INVALID",
                "status": certificate_status, "certificate_sha256": null, "decided_by": null
            }],
            "downloads": [{
                "id": "dl_1", "suggested_filename": "report.pdf", "state": "cancelled",
                "received_bytes": 10, "failure": "too_large"
            }],
            "popups_blocked": 2
        }],
        "in_doubt": [{ "call_id": "tc_1", "action_id": "act_1", "reason": "the worker restarted" }]
    })
}

async fn record(
    conn: &mut PgConnection,
    report: &Value,
) -> quansio_machine::control::browser::BrowserSession {
    BrowserSessionStore::record_guard_report(
        conn,
        TENANT,
        SESSION,
        &GuardReport::from_json(report).expect("a valid report"),
        AT,
    )
    .await
    .expect("record the report")
}

fn user() -> DecisionActor {
    DecisionActor::User {
        user_id: USER.to_string(),
    }
}

fn agent() -> DecisionActor {
    DecisionActor::Agent {
        identity: "agt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
    }
}

// ------------------------------------------------------------------ the report

#[tokio::test]
async fn a_report_is_parsed_strictly_stored_on_the_row_and_projected() {
    let name = common::scratch_name("guard_report");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;

    let session = record(conn, &report("pending")).await;
    let tab = &session.guard.tabs[0];
    assert_eq!(
        tab.dialog.as_ref().map(|d| d.kind.as_str()),
        Some("confirm")
    );
    assert_eq!(tab.certificates[0].status, "pending");
    assert_eq!(tab.downloads[0].failure.as_deref(), Some("too_large"));
    assert_eq!(tab.popups_blocked, 2);
    assert_eq!(session.guard.in_doubt[0].action_id, "act_1");
    assert_eq!(session.guard.reported_at.as_deref(), Some(AT));
    // The tab list the surface renders carries the revision too.
    assert_eq!(
        session.tabs[0].document_revision.as_deref(),
        Some("rev_0123456789abcdef")
    );

    // A different connection reads it back: the state is on the row, not in the caller.
    let mut other = pool.acquire().await.expect("acquire");
    let reloaded = BrowserSessionStore::load(&mut other, TENANT, SESSION)
        .await
        .expect("load");
    assert_eq!(reloaded.guard, session.guard);
    assert_eq!(
        reloaded.tabs[0].document_revision,
        session.tabs[0].document_revision
    );
    // Moving the tab to another page keeps its revision rather than dropping it.
    let moved = BrowserSessionStore::record_page(
        &mut other,
        TENANT,
        SESSION,
        TAB,
        "https://example.com/b",
        "B",
    )
    .await
    .expect("record");
    assert_eq!(
        moved.tabs[0].document_revision.as_deref(),
        Some("rev_0123456789abcdef")
    );

    // The next report replaces the last.
    let mut next = report("pending");
    next["tabs"][0]["dialog"] = Value::Null;
    next["in_doubt"] = json!([]);
    let replaced = record(conn, &next).await;
    assert!(replaced.guard.tabs[0].dialog.is_none());
    assert!(replaced.guard.in_doubt.is_empty());

    common::drop_pool(&pool, &name).await;
}

#[test]
fn a_malformed_or_oversized_report_is_refused_before_it_is_stored() {
    let valid = report("pending");
    assert!(GuardReport::from_json(&valid).is_ok());

    let mut cases: Vec<(&str, Value)> = vec![
        ("not an object", json!([1, 2])),
        ("no tabs", json!({ "in_doubt": [] })),
    ];
    let mut no_revision = valid.clone();
    no_revision["tabs"][0]["document_revision"] = json!("");
    cases.push(("a tab without a revision", no_revision));
    let mut bad_certificate = valid.clone();
    bad_certificate["tabs"][0]["certificates"][0]["status"] = json!("maybe");
    cases.push(("an undefined certificate status", bad_certificate));
    let mut bad_download = valid.clone();
    bad_download["tabs"][0]["downloads"][0]["state"] = json!("teleporting");
    cases.push(("an undefined download state", bad_download));
    let mut too_many_tabs = valid.clone();
    let tab = valid["tabs"][0].clone();
    too_many_tabs["tabs"] = Value::Array(vec![tab; 65]);
    cases.push(("65 tabs", too_many_tabs));
    let mut too_many_in_doubt = valid.clone();
    too_many_in_doubt["in_doubt"] = Value::Array(vec![
        json!({ "call_id": "a", "action_id": "b", "reason": "c" });
        65
    ]);
    cases.push(("65 in-doubt actions", too_many_in_doubt));
    let mut huge = valid;
    huge["tabs"][0]["dialog"]["message"] = json!("x".repeat(300_000));
    cases.push(("a message past the size bound", huge));

    for (label, case) in cases {
        let refused = GuardReport::from_json(&case);
        assert!(
            matches!(refused, Err(BrowserSessionError::GuardInvalid { .. })),
            "{label}: {refused:?}"
        );
    }
}

// ------------------------------------------------------------------ user-only decisions

#[tokio::test]
async fn only_a_user_decides_a_certificate_and_an_agent_leaves_the_row_untouched() {
    let name = common::scratch_name("guard_cert");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    record(conn, &report("pending")).await;
    let before =
        sqlx::query_scalar::<_, Value>("SELECT guard_state FROM browser_sessions WHERE id = $1")
            .bind(SESSION)
            .fetch_one(&mut *conn)
            .await
            .expect("read");

    // An agent — however it presents itself — is refused, and the row is exactly as it was.
    for actor in [
        agent(),
        DecisionActor::User {
            user_id: "agt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
        },
        DecisionActor::User {
            user_id: String::new(),
        },
    ] {
        for decision in [Decision::Allow, Decision::Deny] {
            let refused = BrowserSessionStore::decide_certificate(
                conn, TENANT, SESSION, TAB, "crt_1", decision, &actor, LATER,
            )
            .await;
            assert!(
                matches!(
                    refused,
                    Err(BrowserSessionError::UserDecisionRequired { .. })
                ),
                "{actor:?} {decision:?}: {refused:?}"
            );
        }
    }
    let after_refusals =
        sqlx::query_scalar::<_, Value>("SELECT guard_state FROM browser_sessions WHERE id = $1")
            .bind(SESSION)
            .fetch_one(&mut *conn)
            .await
            .expect("read");
    assert_eq!(after_refusals, before, "a refused decision changed the row");

    // Another tenant cannot decide it, and neither can anyone for a tab or id that is not pending.
    assert!(matches!(
        BrowserSessionStore::decide_certificate(
            conn,
            OTHER_TENANT,
            SESSION,
            TAB,
            "crt_1",
            Decision::Allow,
            &user(),
            LATER
        )
        .await,
        Err(BrowserSessionError::NotFound(_))
    ));
    for (tab, certificate) in [("OTHER-TAB", "crt_1"), (TAB, "crt_nope")] {
        assert!(matches!(
            BrowserSessionStore::decide_certificate(
                conn,
                TENANT,
                SESSION,
                tab,
                certificate,
                Decision::Allow,
                &user(),
                LATER
            )
            .await,
            Err(BrowserSessionError::DecisionSubjectNotFound(_))
        ));
    }

    // The user decides. The row says who, and the receipt carries it to the worker.
    let receipt = BrowserSessionStore::decide_certificate(
        conn,
        TENANT,
        SESSION,
        TAB,
        "crt_1",
        Decision::Allow,
        &user(),
        LATER,
    )
    .await
    .expect("the user decides");
    assert_eq!(receipt.subject(), "crt_1");
    assert_eq!(receipt.tab_id(), TAB);
    assert_eq!(receipt.session_id(), SESSION);
    assert_eq!(receipt.decision(), Decision::Allow);
    assert_eq!(receipt.decided_by(), USER);
    assert_eq!(receipt.decided_at(), LATER);
    let session = BrowserSessionStore::load(conn, TENANT, SESSION)
        .await
        .expect("load");
    let certificate = &session.guard.tabs[0].certificates[0];
    assert_eq!(certificate.status, "continued");
    assert_eq!(certificate.decided_by.as_deref(), Some(USER));
    assert_eq!(session.guard.decisions.len(), 1);
    assert_eq!(session.guard.decisions[0].decided_by, USER);

    // A decision is made once: nothing is pending now.
    assert!(matches!(
        BrowserSessionStore::decide_certificate(
            conn,
            TENANT,
            SESSION,
            TAB,
            "crt_1",
            Decision::Deny,
            &user(),
            LATER
        )
        .await,
        Err(BrowserSessionError::DecisionSubjectNotFound(_))
    ));
    // And a worker report that raced the decision cannot put the error back to pending.
    let raced = record(conn, &report("pending")).await;
    assert_eq!(raced.guard.tabs[0].certificates[0].status, "continued");
    assert_eq!(
        raced.guard.tabs[0].certificates[0].decided_by.as_deref(),
        Some(USER)
    );
    assert_eq!(
        raced.guard.decisions.len(),
        1,
        "a report rewrote the user's decisions"
    );

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn only_a_user_grants_media_and_only_camera_and_microphone_can_be_granted() {
    let name = common::scratch_name("guard_permission");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    let origin = "https://meet.example.com";

    let refused = BrowserSessionStore::decide_permission(
        conn,
        TENANT,
        SESSION,
        TAB,
        origin,
        "camera",
        Decision::Allow,
        &agent(),
        AT,
    )
    .await;
    assert!(
        matches!(
            refused,
            Err(BrowserSessionError::UserDecisionRequired { .. })
        ),
        "{refused:?}"
    );
    assert!(BrowserSessionStore::load(conn, TENANT, SESSION)
        .await
        .expect("load")
        .guard
        .decisions
        .is_empty());

    // Everything but camera and microphone is not a decision at all, even for a user.
    for permission in ["geolocation", "notifications", "clipboard-read", "midi", ""] {
        let refused = BrowserSessionStore::decide_permission(
            conn,
            TENANT,
            SESSION,
            TAB,
            origin,
            permission,
            Decision::Allow,
            &user(),
            AT,
        )
        .await;
        assert!(
            matches!(refused, Err(BrowserSessionError::DecisionNotGrantable(_))),
            "{permission}: {refused:?}"
        );
    }
    // A grant is for one origin: a path, credentials, a wildcard or a non-web scheme is not an origin.
    for bad in [
        "meet.example.com",
        "https://meet.example.com/room",
        "https://u@meet.example.com",
        "ftp://x",
        "*",
        "",
    ] {
        let refused = BrowserSessionStore::decide_permission(
            conn,
            TENANT,
            SESSION,
            TAB,
            bad,
            "camera",
            Decision::Allow,
            &user(),
            AT,
        )
        .await;
        assert!(
            matches!(refused, Err(BrowserSessionError::DecisionNotGrantable(_))),
            "{bad}: {refused:?}"
        );
    }

    let receipt = BrowserSessionStore::decide_permission(
        conn,
        TENANT,
        SESSION,
        TAB,
        origin,
        "camera",
        Decision::Allow,
        &user(),
        AT,
    )
    .await
    .expect("the user grants the camera");
    assert_eq!(receipt.subject(), "https://meet.example.com|camera");
    assert_eq!(receipt.decided_by(), USER);
    BrowserSessionStore::decide_permission(
        conn,
        TENANT,
        SESSION,
        TAB,
        origin,
        "microphone",
        Decision::Deny,
        &user(),
        LATER,
    )
    .await
    .expect("the user denies the microphone");
    let session = BrowserSessionStore::load(conn, TENANT, SESSION)
        .await
        .expect("load");
    let subjects: Vec<(&str, &str)> = session
        .guard
        .decisions
        .iter()
        .map(|d| (d.subject.as_str(), d.decision.as_str()))
        .collect();
    assert_eq!(
        subjects,
        vec![
            ("https://meet.example.com|camera", "allow"),
            ("https://meet.example.com|microphone", "deny")
        ]
    );

    common::drop_pool(&pool, &name).await;
}

#[test]
fn a_receipt_has_no_constructor_outside_the_control_plane_and_the_test_feature() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/control/browser_guard.rs"
    ))
    .expect("read browser_guard.rs");
    // The only places a receipt is built with a struct literal: the two store functions that first
    // check the actor is a user, and the constructor behind the `test-support` feature.
    let built: Vec<usize> = source
        .match_indices("Ok(DecisionReceipt {")
        .map(|(index, _)| index)
        .collect();
    assert_eq!(built.len(), 2, "unexpected receipt constructions");
    let mut functions = Vec::new();
    for index in built {
        let head = &source[..index];
        let function = head
            .rfind("pub async fn ")
            .expect("inside a store function");
        let name = head[function + "pub async fn ".len()..]
            .split('(')
            .next()
            .unwrap_or_default()
            .to_string();
        // That function checks the actor before it builds anything.
        let body = &source[function..index];
        assert!(
            body.contains("require_user("),
            "{name} builds a receipt without requiring a user"
        );
        functions.push(name);
    }
    functions.sort();
    assert_eq!(functions, vec!["decide_certificate", "decide_permission"]);
    let gate = source
        .find("pub fn for_tests")
        .expect("the test constructor exists");
    assert!(
        source[gate.saturating_sub(400)..gate].contains("#[cfg(feature = \"test-support\")]"),
        "the test constructor is not feature-gated"
    );
    // No other public way in.
    assert_eq!(
        source.matches("Result<DecisionReceipt").count(),
        2,
        "another function returns a receipt"
    );
    assert!(!source.contains("impl Default for DecisionReceipt"));
    assert!(!source.contains("derive(Default"));

    // The worker rebuilds a receipt from the control channel (EXEC-009 composition). That is a third way in, and it is
    // pinned the same way: exactly one function does it, it takes the wire description and nothing else, and the only
    // caller in the workspace is the worker's control-channel reader.
    assert_eq!(
        source
            .matches("Ok(Self {\n            session_id: wire.session_id.clone()")
            .count(),
        1,
        "another function builds a receipt from the wire"
    );
    assert_eq!(
        source
            .matches("pub fn from_control_channel(wire: &ReceiptWire)")
            .count(),
        1
    );
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut callers = Vec::new();
    for entry in std::fs::read_dir(&workspace).expect("crates") {
        let src = entry.expect("crate").path().join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            for item in read.flatten() {
                let path = item.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && std::fs::read_to_string(&path)
                        .is_ok_and(|text| text.contains("from_control_channel("))
                {
                    callers.push(
                        path.strip_prefix(&workspace)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
    }
    callers.sort();
    assert_eq!(
        callers,
        vec!["machine/src/control/browser_guard.rs", "qworkerd/src/daemon.rs"],
        "a receipt is rebuilt from the wire somewhere other than the worker's control-channel reader"
    );
}

// ------------------------------------------------------------------ secrets

fn material(allowed: &[&str]) -> Vec<u8> {
    BrowserSecretMaterial::new(
        [
            ("username".to_string(), "alice".to_string()),
            ("password".to_string(), CANARY.to_string()),
        ],
        allowed.iter().map(|origin| (*origin).to_string()).collect(),
    )
    .to_bytes()
}

async fn register(
    conn: &mut PgConnection,
    id: &str,
    label: &str,
    workspace: Option<&str>,
    bytes: &[u8],
) {
    SecretBroker::register(
        conn,
        &provider(),
        &NewSecret {
            id,
            tenant_id: TENANT,
            workspace_id: workspace,
            provider: "browser",
            label,
            expires_at: None,
        },
        bytes,
        &OsEntropy,
    )
    .await
    .expect("register the secret");
}

fn request<'a>(
    name: &'a str,
    field: &'a str,
    workspace: Option<&'a str>,
) -> BrowserSecretRequest<'a> {
    BrowserSecretRequest {
        tenant_id: TENANT,
        workspace_id: workspace,
        name,
        field,
        actor: USER,
        effect_id: EFFECT,
        at: AT,
        expires_at: EXPIRES,
    }
}

#[tokio::test]
async fn a_placeholder_resolves_through_the_brokers_handles_and_the_value_is_stored_nowhere_in_clear(
) {
    let name = common::scratch_name("guard_secret");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    register(
        conn,
        "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        "acme",
        Some(WORKSPACE),
        &material(&["https://login.acme.example"]),
    )
    .await;

    let (value, access) = BrowserSecrets::resolve(
        conn,
        &provider(),
        &request("acme", "password", Some(WORKSPACE)),
    )
    .await
    .expect("resolve");
    assert_eq!(value.expose(), CANARY);
    assert_eq!(
        value.allowed_origins(),
        ["https://login.acme.example".to_string()]
    );
    // The materialization the broker audits is scoped to the typing tool and names the effect it settles.
    assert_eq!(access.scope, format!("tool:{BROWSER_SECRET_TOOL}"));
    assert_eq!(access.effect_id, EFFECT);
    assert_eq!(access.decision, "allowed");
    let (username, _) = BrowserSecrets::resolve(
        conn,
        &provider(),
        &request("acme", "username", Some(WORKSPACE)),
    )
    .await
    .expect("resolve");
    assert_eq!(username.expose(), "alice");

    // No rendering of anything the control plane holds carries the value.
    for rendering in [
        format!("{value:?}"),
        format!("{access:?}"),
        format!(
            "{:?}",
            BrowserSecretMaterial::parse(&material(&["x"])).expect("parse")
        ),
    ] {
        assert!(!rendering.contains(CANARY), "{rendering}");
    }
    // Every column of every row that could hold it: only the sealed ciphertext differs from the label.
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT (to_jsonb(s) - 'ciphertext')::text FROM secret_handles s UNION ALL \
         SELECT to_jsonb(b)::text FROM browser_sessions b UNION ALL \
         SELECT to_jsonb(c)::text FROM commands c",
    )
    .fetch_all(&mut *conn)
    .await
    .expect("scan");
    assert!(!rows.is_empty());
    for row in &rows {
        assert!(!row.contains(CANARY), "the value is stored in clear: {row}");
    }
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT ciphertext FROM secret_handles WHERE id = $1")
        .bind("sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA")
        .fetch_one(&mut *conn)
        .await
        .expect("ciphertext");
    assert!(!sealed
        .windows(CANARY.len())
        .any(|window| window == CANARY.as_bytes()));

    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn an_unresolvable_placeholder_is_refused_and_says_why_without_the_value() {
    let name = common::scratch_name("guard_secret_refusals");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    let mut pooled = seed(&pool).await;
    let conn: &mut PgConnection = &mut pooled;
    register(
        conn,
        "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        "acme",
        Some(WORKSPACE),
        &material(&["https://login.acme.example"]),
    )
    .await;
    register(
        conn,
        "sec_01J8Z3K6F1N8VQ2X5W9Y0BBBBB",
        "twin",
        None,
        &material(&["https://a.example"]),
    )
    .await;
    register(
        conn,
        "sec_01J8Z3K6F1N8VQ2X5W9Y0CCCCC",
        "twin",
        None,
        &material(&["https://b.example"]),
    )
    .await;
    register(
        conn,
        "sec_01J8Z3K6F1N8VQ2X5W9Y0DDDDD",
        "plain",
        None,
        b"just a string, not a browser secret",
    )
    .await;

    // No such name.
    assert!(matches!(
        BrowserSecrets::resolve(
            conn,
            &provider(),
            &request("nobody", "password", Some(WORKSPACE))
        )
        .await,
        Err(BrowserSecretError::NotFound(_))
    ));
    // No such field.
    let missing =
        BrowserSecrets::resolve(conn, &provider(), &request("acme", "pin", Some(WORKSPACE))).await;
    assert!(
        matches!(missing, Err(BrowserSecretError::FieldMissing { .. })),
        "{missing:?}"
    );
    // A secret of another workspace is not found for this one.
    assert!(matches!(
        BrowserSecrets::resolve(
            conn,
            &provider(),
            &request("acme", "password", Some(OTHER_WORKSPACE))
        )
        .await,
        Err(BrowserSecretError::NotFound(_))
    ));
    // Two active handles with one name make the placeholder a guess, so it is refused.
    assert!(matches!(
        BrowserSecrets::resolve(conn, &provider(), &request("twin", "password", None)).await,
        Err(BrowserSecretError::Ambiguous(_))
    ));
    // A handle whose material is not a browser secret is refused as malformed.
    let malformed =
        BrowserSecrets::resolve(conn, &provider(), &request("plain", "password", None)).await;
    assert!(
        matches!(malformed, Err(BrowserSecretError::Malformed(_))),
        "{malformed:?}"
    );
    for refusal in [
        format!(
            "{:?}",
            BrowserSecrets::resolve(conn, &provider(), &request("acme", "pin", Some(WORKSPACE)))
                .await
                .err()
        ),
        format!(
            "{:?}",
            BrowserSecrets::resolve(conn, &provider(), &request("plain", "password", None))
                .await
                .err()
        ),
    ] {
        assert!(!refusal.contains(CANARY));
    }

    // Revoking the handle ends it: it is not an active handle any more, and rotation re-seals the value.
    SecretBroker::revoke(conn, TENANT, "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA")
        .await
        .expect("revoke");
    assert!(matches!(
        BrowserSecrets::resolve(
            conn,
            &provider(),
            &request("acme", "password", Some(WORKSPACE))
        )
        .await,
        Err(BrowserSecretError::NotFound(_))
    ));

    common::drop_pool(&pool, &name).await;
}

#[test]
fn browser_secret_material_round_trips_and_has_no_value_in_its_rendering() {
    let bytes = material(&["https://login.acme.example", "https://sso.acme.example"]);
    let parsed = BrowserSecretMaterial::parse(&bytes).expect("parse");
    assert_eq!(parsed.field("password"), Some(CANARY));
    assert_eq!(parsed.field("missing"), None);
    assert_eq!(parsed.allowed_origins().len(), 2);
    assert!(!format!("{parsed:?}").contains(CANARY));
    assert!(BrowserSecretMaterial::parse(b"not json").is_err());
    assert!(BrowserSecretMaterial::parse(br#"{"fields": {"a": 1}}"#).is_err());
    // A secret with no origins is allowed nowhere.
    let nowhere = BrowserSecretMaterial::parse(&material(&[])).expect("parse");
    assert!(nowhere.allowed_origins().is_empty());
}
